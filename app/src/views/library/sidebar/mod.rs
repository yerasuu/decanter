//! The left-hand game list: a search row with a filter toggle, a Steam-
//! style filter panel, then the list grouped into shelves.
//!
//! Filter semantics follow Steam's: leaving a section empty means "no
//! constraint", checks inside a section combine, and sections combine with
//! each other. GÉNERO and LOGROS are alternatives, so they're OR'd; within
//! ESTADO they're independent properties of one game (installed, unplayed,
//! favourite), so they're AND'd — checking two there narrows rather than
//! widens. Product type is a single choice, not a section.

mod filter_panel;
mod rows;
mod scrollbar;

use std::collections::HashSet;

use gpui::{
    App, ClickEvent, Context, Entity, FocusHandle, Focusable as _, FontWeight, ListAlignment, ListState, Pixels,
    SharedString, Stateful, Subscription, Window, div, list, prelude::*, px, rgb,
};

use rust_i18n::t;

use super::data::{self, Collections, LibraryStats};
use super::filters::{Filters, KindSelection, PlayState, SavedFilter, Sort};
use super::{LibraryModel, LibraryPopup};
use crate::i18n::Text;
use crate::state::AppState;
use crate::theme;
use crate::widgets::select::SelectStyle;
use crate::widgets::{Emoji, InputEvent, Select, TextInput, emoji, tooltip};
use rows::{Row, RowsKey};

const SIDEBAR_WIDTH: f32 = 292.;

type CacheKey = (String, u32, u32);

/// Per-profile sqlite reads, kept until something they depend on moves —
/// the sidebar re-renders on every artwork arrival and install tick. Read
/// on a background thread; the previous copy stays on screen meanwhile.
#[derive(Default)]
struct Cache {
    key: Option<CacheKey>,
    favorites: HashSet<String>,
    hidden: HashSet<String>,
    stats: LibraryStats,
    collections: Collections,
    accounts: Vec<(String, String)>,
}

impl Cache {
    fn load(key: CacheKey) -> Self {
        let profile_id = key.0.as_str();
        Self {
            favorites: data::favorite_ids_in_profile(profile_id),
            hidden: data::hidden_ids_in_profile(profile_id),
            stats: data::library_stats(profile_id),
            collections: data::load_collections(profile_id),
            accounts: data::gog_accounts_with_names(profile_id),
            key: Some(key),
        }
    }
}

pub struct SidebarView {
    model: Entity<LibraryModel>,
    search: Entity<TextInput>,
    kind: KindSelection,
    panel_open: bool,
    panel_focus: FocusHandle,
    collapsed: HashSet<String>,
    /// Which profile's view state is loaded, rather than a plain "loaded"
    /// flag: the switcher changes the profile without rebuilding this
    /// view, and one profile's arrangement must not be written over the
    /// other's. Doubles as the "restore has run" guard for saving.
    loaded_profile: Option<String>,
    /// The collection header being dragged, dimmed while it is.
    dragging_shelf: Option<String>,
    /// Ctrl-click builds this up; a plain click clears it.
    marked: HashSet<String>,
    rename: Option<(String, Entity<TextInput>, Subscription)>,
    saving: bool,
    save_name: Entity<TextInput>,
    cache: Cache,
    cache_loading: Option<CacheKey>,
    rows: Vec<Row>,
    rows_key: Option<RowsKey>,
    /// Collections in display order — what a drag-reorder drop needs to
    /// turn "dropped on this shelf" into a delta.
    order: Vec<String>,
    list: ListState,
    /// Where on the scrollbar thumb the pointer took hold of it, while it's
    /// being dragged.
    thumb_grab: Option<Pixels>,
}

impl SidebarView {
    pub fn new(model: Entity<LibraryModel>, _: &mut Window, cx: &mut Context<Self>) -> Self {
        let state = AppState::get(cx).clone();
        cx.observe(&state.library, |_, _, cx| cx.notify()).detach();
        cx.observe(&state.session, |_, _, cx| cx.notify()).detach();
        cx.observe(&state.background, |_, _, cx| cx.notify()).detach();
        cx.observe(&model, |_, _, cx| cx.notify()).detach();
        let search = cx.new(|cx| TextInput::new(Text::new("library.sidebar.search"), cx).text_size(12.));
        cx.subscribe(&search, |_, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Changed) {
                cx.notify();
            }
        })
        .detach();
        let save_name = cx.new(|cx| TextInput::new(Text::new("library.collections.name_placeholder"), cx));
        cx.subscribe(&save_name, |this, input, event: &InputEvent, cx| match event {
            InputEvent::Submit => this.save_dynamic_collection(&input, cx),
            InputEvent::Cancel => {
                this.saving = false;
                cx.notify();
            }
            InputEvent::Changed => {}
        })
        .detach();
        let list = ListState::new(0, ListAlignment::Top, px(400.));
        // The scrollbar is drawn from the list's offset, so a wheel scroll
        // has to re-render this view and not only the list.
        let this = cx.entity().downgrade();
        list.set_scroll_handler(move |_, _, cx| {
            let _ = this.update(cx, |_, cx| cx.notify());
        });
        Self {
            model,
            search,
            kind: KindSelection::default(),
            panel_open: false,
            panel_focus: cx.focus_handle(),
            collapsed: HashSet::new(),
            loaded_profile: None,
            dragging_shelf: None,
            marked: HashSet::new(),
            rename: None,
            saving: false,
            save_name,
            cache: Cache::default(),
            cache_loading: None,
            rows: Vec::new(),
            rows_key: None,
            order: Vec::new(),
            list,
            thumb_grab: None,
        }
    }

    fn profile(cx: &App) -> Option<String> {
        AppState::get(cx).session.read(cx).active_profile.clone()
    }

    /// Restored once per profile rather than on every render, so toggling
    /// a shelf (or a filter) isn't immediately undone by a reload.
    fn restore(&mut self, cx: &mut Context<Self>) {
        let Some(id) = Self::profile(cx) else {
            return;
        };
        if self.loaded_profile.as_deref() == Some(id.as_str()) {
            return;
        }
        self.collapsed = data::collapsed_shelves(&id);
        // A filter state that no longer parses leaves the defaults alone
        // rather than half-applying itself.
        match data::view_state(&id, data::SIDEBAR_FILTERS)
            .as_deref()
            .map(SavedFilter::decode)
        {
            Some(Some(saved)) => {
                self.kind = saved.kind;
                self.model.update(cx, |model, _| model.filters = saved.filters);
            }
            Some(None) => tracing::warn!(
                profile_id = id,
                "failed to decode saved sidebar filters, using defaults"
            ),
            None => {}
        }
        self.panel_open = data::view_state(&id, data::FILTER_PANEL).as_deref() == Some("1");
        self.loaded_profile = Some(id);
    }

    /// Saved through the same encoding a dynamic collection uses, so "save
    /// this filter as a collection" and "restore what I was looking at"
    /// can't disagree.
    fn persist(&self, cx: &App) {
        let Some(id) = self.loaded_profile.as_deref() else {
            return;
        };
        let saved = SavedFilter {
            kind: self.kind,
            filters: self.model.read(cx).filters.clone(),
        };
        if let Some(encoded) = saved.encode() {
            data::save_view_state(id, data::SIDEBAR_FILTERS, &encoded);
        } else {
            tracing::warn!(profile_id = %id, "failed to encode sidebar filters, not saving");
        }
        data::save_view_state(id, data::FILTER_PANEL, if self.panel_open { "1" } else { "0" });
    }

    fn update_filters(&mut self, cx: &mut Context<Self>, change: impl FnOnce(&mut Filters)) {
        self.model.update(cx, |model, cx| {
            change(&mut model.filters);
            cx.notify();
        });
        self.persist(cx);
        cx.notify();
    }

    fn refresh_cache(&mut self, profile_id: &str, cx: &mut Context<Self>) {
        let background = AppState::get(cx).background.read(cx);
        // Playtime too: otherwise a session's playtime only shows up here
        // after some unrelated re-render.
        let key = (
            profile_id.to_owned(),
            background.organisation,
            background.playtime_generation,
        );
        if self.cache.key.as_ref() == Some(&key) || self.cache_loading.as_ref() == Some(&key) {
            return;
        }
        self.cache_loading = Some(key.clone());
        let load = cx.background_spawn(async move { Cache::load(key) });
        cx.spawn(async move |this, cx| {
            let loaded = load.await;
            let _ = this.update(cx, |this, cx| {
                // A newer request supersedes this one.
                if this.cache_loading == loaded.key {
                    this.cache_loading = None;
                    this.cache = loaded;
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn set_panel(&mut self, open: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.panel_open = open;
        if open {
            // Opening the panel tidies away any stray right-click menu
            // rather than sitting behind or beside it.
            self.model.update(cx, |model, cx| model.close_popup(cx));
            window.focus(&self.panel_focus, cx);
        }
        self.persist(cx);
        cx.notify();
    }

    fn save_dynamic_collection(&mut self, input: &Entity<TextInput>, cx: &mut Context<Self>) {
        let name = input.read(cx).text().trim().to_owned();
        let Some(profile_id) = Self::profile(cx) else {
            return;
        };
        if name.is_empty() {
            return;
        }
        // Stores the filter itself, not the games it matches right now —
        // that is what makes it dynamic.
        let saved = SavedFilter {
            kind: self.kind,
            filters: self.model.read(cx).filters.clone(),
        };
        if let Some(encoded) = saved.encode() {
            data::create_dynamic_collection(&profile_id, &name, &encoded);
            super::actions::bump_organisation(cx);
        } else {
            tracing::warn!(profile_id, name, "failed to encode filter, collection not saved");
        }
        input.update(cx, |input, cx| input.set_text("", cx));
        self.saving = false;
        cx.notify();
    }

    /// Picks up a "Renombrar" from the shelf menu: a name field over that
    /// shelf, Enter saves, Escape drops it.
    fn sync_rename(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let wanted = self.model.read(cx).renaming.clone();
        match (wanted, &self.rename) {
            (None, Some(_)) => self.rename = None,
            (Some((id, title)), current) if current.as_ref().is_none_or(|(current, ..)| *current != id) => {
                let input = cx.new(|cx| {
                    let mut input = TextInput::new("", cx).text_size(12.).padding(7., 5.);
                    input.set_text(title, cx);
                    input
                });
                window.focus(&input.focus_handle(cx), cx);
                let collection_id = id.clone();
                let subscription = cx.subscribe(&input, move |this, input, event: &InputEvent, cx| match event {
                    InputEvent::Submit => {
                        let name = input.read(cx).text().trim().to_owned();
                        if !name.is_empty() {
                            data::rename_collection(&collection_id, &name);
                            super::actions::bump_organisation(cx);
                        }
                        this.finish_rename(cx);
                    }
                    InputEvent::Cancel => this.finish_rename(cx),
                    InputEvent::Changed => {}
                });
                self.rename = Some((id, input, subscription));
            }
            _ => {}
        }
    }

    fn finish_rename(&mut self, cx: &mut Context<Self>) {
        self.rename = None;
        self.model.update(cx, |model, cx| {
            model.renaming = None;
            cx.notify();
        });
    }

    fn header(&self, active_filters: usize, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        div()
            .flex_none()
            .px(px(14.))
            .py_3()
            .border_b_1()
            .border_color(rgb(theme::LINE))
            .child(self.kind_row(cx))
            .child(self.search_row(active_filters, cx))
    }

    fn kind_row(&self, cx: &Context<Self>) -> gpui::Div {
        let filters = self.model.read(cx).filters.clone();
        let recent = filters.sort == Sort::RecentlyPlayed;
        let installed_only = filters.states.contains(&PlayState::Installed);
        let entity = cx.entity();
        let kind_options = [
            ("games", t!("library.sidebar.kind.games")),
            ("games_extras", t!("library.sidebar.kind.games_extras")),
            ("extras", t!("library.sidebar.kind.extras")),
            ("movies", t!("library.sidebar.kind.movies")),
            ("all", t!("library.sidebar.kind.all")),
        ]
        .map(|(value, label)| (SharedString::from(value), SharedString::from(label)));
        div()
            .flex()
            .items_center()
            .gap(px(6.))
            .mb_2()
            .child(
                div().flex_1().min_w_0().child(
                    Select::new("kind-select", kind_options, self.kind.value())
                        .style(SelectStyle::Kind)
                        .full_width()
                        .on_change(move |value, _, cx| {
                            entity.update(cx, |this, cx| {
                                this.kind = KindSelection::from_value(value);
                                this.persist(cx);
                                cx.notify();
                            });
                        }),
                ),
            )
            // Shortcuts to two panel settings that get reached for
            // constantly. They toggle the same state the panel edits, so
            // the two never disagree.
            .child(
                quick_toggle("quick-recent", emoji(Emoji::Fire, 13.), recent)
                    .tooltip(tooltip(t!("library.sidebar.sort_recent")))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.update_filters(cx, |filters| {
                            filters.sort = if filters.sort == Sort::RecentlyPlayed {
                                Sort::default()
                            } else {
                                Sort::RecentlyPlayed
                            };
                        });
                    })),
            )
            .child(
                quick_toggle("quick-installed", "▶", installed_only)
                    .tooltip(tooltip(t!("library.sidebar.installed_only")))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.update_filters(cx, |filters| {
                            if !filters.states.insert(PlayState::Installed) {
                                filters.states.remove(&PlayState::Installed);
                            }
                        });
                    })),
            )
            .child(
                quick_toggle("quick-new-collection", "+", false)
                    .tooltip(tooltip(t!("library.collections.new")))
                    .on_click(cx.listener(|this, event: &ClickEvent, _, cx| {
                        let position = event.position();
                        this.model.update(cx, |model, cx| {
                            model.open_popup(LibraryPopup::NewCollection(position), cx);
                        });
                    })),
            )
    }

    fn search_row(&self, active_filters: usize, cx: &Context<Self>) -> gpui::Div {
        let has_search = !self.search.read(cx).text().is_empty();
        let filter_active = self.panel_open || active_filters > 0;
        div()
            .flex()
            .items_center()
            .gap(px(6.))
            .child(div().flex_1().min_w_0().child(self.search.clone()))
            .when(has_search, |row| {
                row.child(
                    search_button("search-clear", false)
                        .tooltip(tooltip(t!("library.sidebar.clear_search")))
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.search.update(cx, |search, cx| search.set_text("", cx));
                            cx.notify();
                        }))
                        .child("✕"),
                )
            })
            .child(
                search_button("filter-toggle", filter_active)
                    .tooltip(tooltip(t!("library.sidebar.filters")))
                    .on_click(cx.listener(|this, _, window, cx| {
                        let open = this.panel_open;
                        this.set_panel(!open, window, cx);
                    }))
                    .child("☰")
                    .when(active_filters > 0, |toggle| {
                        toggle.child(
                            theme::mono()
                                .ml_1()
                                .px(px(5.))
                                .rounded(px(8.))
                                .bg(rgb(theme::ACCENT_BLUE))
                                .text_color(rgb(theme::TEXT_ON_BLUE))
                                .text_size(px(10.))
                                .font_weight(FontWeight::SEMIBOLD)
                                .child(active_filters.to_string()),
                        )
                    }),
            )
    }
}

/// `.icon-btn.quick-toggle`: a 13px glyph that fills on hover and keeps a
/// blue border while its setting is on.
fn quick_toggle(id: &'static str, glyph: impl IntoElement, active: bool) -> Stateful<gpui::Div> {
    div()
        .id(id)
        .flex_none()
        .px_2()
        .py(px(5.))
        .border_1()
        .rounded(px(theme::RADIUS_SM))
        .text_size(px(13.))
        .line_height(px(13.))
        .text_color(rgb(theme::TEXT_MUTED))
        .cursor_pointer()
        .border_color(if active {
            rgb(theme::ACCENT_BLUE).into()
        } else {
            gpui::transparent_black()
        })
        .when(active, |toggle| toggle.bg(rgb(theme::BG_PANEL)))
        .hover(|toggle| toggle.text_color(rgb(theme::TEXT_PRIMARY)).bg(rgb(theme::BG_PANEL)))
        .child(glyph)
}

/// `.search-clear` / `.filter-toggle`: same family as the quick toggles,
/// but the active state colours the glyph instead of filling.
fn search_button(id: &'static str, active: bool) -> Stateful<gpui::Div> {
    div()
        .id(id)
        .flex()
        .flex_none()
        .items_center()
        .px(px(10.))
        .py(px(6.))
        .border_1()
        .rounded(px(theme::RADIUS_SM))
        .text_size(px(13.))
        .line_height(px(13.))
        .cursor_pointer()
        .border_color(if active {
            rgb(theme::ACCENT_BLUE).into()
        } else {
            gpui::transparent_black()
        })
        .text_color(rgb(if active { theme::ACCENT_BLUE } else { theme::TEXT_MUTED }))
        .hover(|button| button.text_color(rgb(theme::TEXT_PRIMARY)).bg(rgb(theme::BG_PANEL)))
}

impl Render for SidebarView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.restore(cx);
        self.sync_rename(window, cx);
        if !cx.has_active_drag() {
            self.dragging_shelf = None;
        }
        if let Some(profile_id) = Self::profile(cx) {
            self.refresh_cache(&profile_id, cx);
        }
        self.rebuild_rows(cx);
        let active_filters = self.model.read(cx).filters.active_count();

        div()
            .w(px(SIDEBAR_WIDTH))
            .flex_none()
            .h_full()
            .flex()
            .flex_col()
            .overflow_hidden()
            .bg(rgb(theme::BG_PANEL_3))
            .border_r_1()
            .border_color(rgb(theme::LINE))
            .on_mouse_move(cx.listener(Self::drag_thumb))
            .child(self.header(active_filters, cx))
            .child(
                div()
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .child(
                        list(
                            self.list.clone(),
                            cx.processor(|this, index, _, cx| this.render_row(index, cx)),
                        )
                        .size_full(),
                    )
                    .children(self.scrollbar(cx)),
            )
            .child(mods_footer(&self.model))
            .child(add_game_footer())
    }
}

fn mods_footer(model: &Entity<LibraryModel>) -> impl IntoElement {
    let model = model.clone();
    footer_row("open-gog-mods")
        .on_click(move |_, _, cx| model.update(cx, |model, cx| model.show_mods(true, cx)))
        .child(div().text_size(px(13.)).child("◆"))
        .child(t!("library.mods.open_list"))
}

/// Steam's "+ Add a Game", at the foot of the list the same way.
fn add_game_footer() -> impl IntoElement {
    footer_row("add-external-game")
        .on_click(|_, _, cx| crate::views::external::open(None, cx))
        .child(div().text_size(px(16.)).child("+"))
        .child(t!("library.sidebar.add_game"))
}

fn footer_row(id: &'static str) -> Stateful<gpui::Div> {
    div()
        .id(id)
        .flex()
        .flex_none()
        .items_center()
        .gap(px(8.))
        .px(px(14.))
        .py(px(10.))
        .border_t_1()
        .border_color(rgb(theme::LINE))
        .text_size(px(12.5))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(rgb(theme::TEXT_MUTED))
        .cursor_pointer()
        .hover(|row| row.bg(rgb(theme::BG_PANEL_2)).text_color(rgb(theme::TEXT_BRIGHT)))
}
