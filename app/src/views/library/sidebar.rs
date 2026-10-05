//! The left-hand game list: a search row with a filter toggle, a Steam-
//! style filter panel, then the list grouped into shelves.
//!
//! Filter semantics follow Steam's: leaving a section empty means "no
//! constraint", checks inside a section combine, and sections combine with
//! each other. GÉNERO and LOGROS are alternatives, so they're OR'd; within
//! ESTADO they're independent properties of one game (installed, unplayed,
//! favourite), so they're AND'd — checking two there narrows rather than
//! widens. Product type is a single choice, not a section.

use std::collections::{BTreeSet, HashMap, HashSet};

use decanter_core::{Game, Storefront};
use decanter_gog::LibraryEntry;
use gpui::{
    AnyElement, App, BoxShadow, ClickEvent, Context, Entity, FocusHandle, Focusable as _, FontWeight, KeyDownEvent,
    ListAlignment, ListState, MouseButton, MouseDownEvent, ObjectFit, SharedString, Stateful, Subscription, Window,
    div, img, list, point, prelude::*, px, rgb,
};

use super::context_menu::{MenuTarget, auto_tag};
use super::data::{self, Collections, LibraryStats};
use super::filters::{
    AchievementState, Filters, KindSelection, PlayState, SavedFilter, Sort, belongs_in_the_library, is_installed,
    pending_install_ids, sort_entries,
};
use super::shelf_popups::ShelfMenuTarget;
use super::shelves;
use super::{LibraryModel, LibraryPopup};
use crate::state::{AppState, LibraryState};
use crate::theme;
use crate::widgets::select::SelectStyle;
use crate::widgets::{Emoji, InputEvent, Select, TextInput, checkbox, emoji, tooltip};

const SIDEBAR_WIDTH: f32 = 292.;

/// How many entries sit behind each filter option.
///
/// Counted against the chosen product type only, never against the other
/// filters — a count that shrank as you ticked boxes elsewhere would stop
/// answering the question it's there for: is this option worth ticking?
#[derive(Default)]
struct Tallies {
    states: HashMap<PlayState, usize>,
    achievements: HashMap<AchievementState, usize>,
    genres: HashMap<String, usize>,
    collections: HashMap<String, usize>,
    storefronts: HashMap<Storefront, usize>,
    accounts: HashMap<String, usize>,
}

fn count_options(
    entries: &[&LibraryEntry],
    favorites: &HashSet<String>,
    stats: &LibraryStats,
    collections: &Collections,
    pending_installs: &HashSet<String>,
) -> Tallies {
    let mut tallies = Tallies::default();
    for entry in entries {
        let id = &entry.game.id;
        let played = stats.seconds(id) > 0;
        for (state, holds) in [
            (PlayState::Installed, is_installed(&entry.game, pending_installs)),
            (PlayState::Played, played),
            (PlayState::Unplayed, !played),
            (PlayState::Favorite, favorites.contains(id)),
        ] {
            if holds {
                *tallies.states.entry(state).or_default() += 1;
            }
        }
        *tallies
            .achievements
            .entry(AchievementState::of(stats.completion(id)))
            .or_default() += 1;
        for genre in &entry.game.genres {
            *tallies.genres.entry(genre.clone()).or_default() += 1;
        }
        for collection in &collections.list {
            if collections.contains(&collection.id, id) {
                *tallies.collections.entry(collection.id.clone()).or_default() += 1;
            }
        }
        *tallies.storefronts.entry(entry.game.storefront).or_default() += 1;
        *tallies.accounts.entry(entry.game.account_id.clone()).or_default() += 1;
    }
    tallies
}

/// Every storefront an entry in the library actually belongs to — only
/// ever `[Gog]` today, but drawn from the entries rather than hardcoded so
/// the next storefront needs no change here either.
fn available_storefronts(entries: &[&LibraryEntry]) -> Vec<Storefront> {
    let mut found: Vec<Storefront> = entries
        .iter()
        .map(|entry| entry.game.storefront)
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();
    found.sort_by_key(|storefront| storefront.as_str_name());
    found
}

const fn storefront_label(storefront: Storefront) -> &'static str {
    match storefront {
        Storefront::Unspecified => "Sin tienda",
        Storefront::Steam => "Steam",
        Storefront::EpicGames => "Epic Games",
        Storefront::Gog => "GOG",
        Storefront::AmazonGames => "Amazon Games",
        Storefront::EaApp => "EA App",
        Storefront::UbisoftConnect => "Ubisoft Connect",
        Storefront::BattleNet => "Battle.net",
    }
}

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

/// Everything the list's rows are derived from; the rows are rebuilt only
/// when one of these moves, not on every render.
#[derive(PartialEq)]
struct RowsKey {
    revision: u64,
    kind: KindSelection,
    query: String,
    filters: Filters,
    cache: Option<CacheKey>,
    pending_installs: HashSet<String>,
    collapsed: HashSet<String>,
    renaming: Option<String>,
    state: LibraryState,
}

struct ShelfHead {
    shelf_id: String,
    title: String,
    collection_id: Option<String>,
    dynamic: bool,
    installed: usize,
    total: usize,
    collapsed: bool,
}

/// One line of the virtual list: the shelves flattened, so only the rows
/// on screen are laid out — and only their icons fetched.
enum Row {
    Message(&'static str),
    Error(String),
    Rename,
    Head(ShelfHead),
    /// `shelf` is the owning collection, so a dragged shelf dims whole.
    Item {
        game: Game,
        installed: bool,
        shelf: Option<String>,
    },
    Empty {
        shelf: Option<String>,
    },
    /// `.library-list`'s 8px bottom padding.
    Spacer,
}

/// A game row being dragged onto a collection's header to join it.
#[derive(Clone)]
struct DraggedGame {
    id: String,
    title: SharedString,
}

/// A collection's header being dragged to reorder it — told apart from a
/// game by type, so the two drags never get mixed up.
#[derive(Clone)]
struct DraggedShelf {
    id: String,
    title: SharedString,
}

/// What follows the pointer during a drag: the row's title, the stand-in
/// for the webview's own drag ghost.
struct DragGhost(SharedString);

impl Render for DragGhost {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .px_3()
            .py_1()
            .bg(rgb(theme::BG_PANEL_2))
            .border_1()
            .border_color(rgb(theme::LINE))
            .rounded(px(theme::RADIUS_SM))
            .opacity(0.85)
            .text_size(px(12.5))
            .text_color(rgb(theme::TEXT_PRIMARY))
            .child(self.0.clone())
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
}

impl SidebarView {
    pub fn new(model: Entity<LibraryModel>, _: &mut Window, cx: &mut Context<Self>) -> Self {
        let state = AppState::get(cx).clone();
        cx.observe(&state.library, |_, _, cx| cx.notify()).detach();
        cx.observe(&state.session, |_, _, cx| cx.notify()).detach();
        cx.observe(&state.background, |_, _, cx| cx.notify()).detach();
        cx.observe(&model, |_, _, cx| cx.notify()).detach();
        let search = cx.new(|cx| TextInput::new("Buscar...", cx).text_size(12.));
        cx.subscribe(&search, |_, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Changed) {
                cx.notify();
            }
        })
        .detach();
        let save_name = cx.new(|cx| TextInput::new("Nombre de la coleccion", cx));
        cx.subscribe(&save_name, |this, input, event: &InputEvent, cx| match event {
            InputEvent::Submit => this.save_dynamic_collection(&input, cx),
            InputEvent::Cancel => {
                this.saving = false;
                cx.notify();
            }
            InputEvent::Changed => {}
        })
        .detach();
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
            list: ListState::new(0, ListAlignment::Top, px(400.)),
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
        let filters = self.model.read(cx).filters.clone();
        let recent = filters.sort == Sort::RecentlyPlayed;
        let installed_only = filters.states.contains(&PlayState::Installed);
        let entity = cx.entity();
        let kind_options = [
            ("games", "Juegos"),
            ("games_extras", "Juegos y extras"),
            ("extras", "Extras"),
            ("movies", "Peliculas"),
            ("all", "Todo"),
        ]
        .map(|(value, label)| (SharedString::from(value), SharedString::from(label)));
        let has_search = !self.search.read(cx).text().is_empty();
        let filter_active = self.panel_open || active_filters > 0;

        let kind_row = div()
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
                    .tooltip(tooltip("Ordenar por actividad reciente"))
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
                    .tooltip(tooltip("Mostrar solo instalados"))
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
                    .tooltip(tooltip("Nueva coleccion"))
                    .on_click(cx.listener(|this, event: &ClickEvent, _, cx| {
                        let position = event.position();
                        this.model.update(cx, |model, cx| {
                            model.open_popup(LibraryPopup::NewCollection(position), cx);
                        });
                    })),
            );

        let search_row = div()
            .flex()
            .items_center()
            .gap(px(6.))
            .child(div().flex_1().min_w_0().child(self.search.clone()))
            .when(has_search, |row| {
                row.child(
                    search_button("search-clear", false)
                        .tooltip(tooltip("Limpiar busqueda"))
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.search.update(cx, |search, cx| search.set_text("", cx));
                            cx.notify();
                        }))
                        .child("✕"),
                )
            })
            .child(
                search_button("filter-toggle", filter_active)
                    .tooltip(tooltip("Filtros"))
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
            );

        div()
            .flex_none()
            .px(px(14.))
            .py_3()
            .border_b_1()
            .border_color(rgb(theme::LINE))
            .child(kind_row)
            .child(search_row)
    }

    #[expect(
        clippy::too_many_lines,
        reason = "one shelf's header, its rename field and its rows, each wired for clicks, menus and \
                  both kinds of drop; splitting it would only scatter the shared captures"
    )]
    fn rebuild_rows(&mut self, cx: &App) {
        let state = AppState::get(cx);
        let library = state.library.read(cx);
        let key = RowsKey {
            revision: library.revision,
            kind: self.kind,
            query: self.search.read(cx).text().to_lowercase(),
            filters: self.model.read(cx).filters.clone(),
            cache: self.cache.key.clone(),
            // A queued or downloading game should already read as
            // installed here, not just once the manifest scan catches up.
            pending_installs: pending_install_ids(&state.background.read(cx).installing),
            collapsed: self.collapsed.clone(),
            renaming: self.rename.as_ref().map(|(id, ..)| id.clone()),
            state: library.state.clone(),
        };
        if self.rows_key.as_ref() == Some(&key) {
            return;
        }
        let cache = &self.cache;
        let mut rows = Vec::new();
        match &key.state {
            LibraryState::Idle => rows.push(Row::Message("Elegi un perfil arriba.")),
            LibraryState::Failed(err) => rows.push(Row::Error(err.clone())),
            // `Loading` never reaches here — the library page shows its own
            // full-screen splash for that.
            LibraryState::Loading | LibraryState::Loaded => {
                let mut visible: Vec<LibraryEntry> = library
                    .games
                    .iter()
                    .filter(|entry| key.kind.accepts(entry.kind))
                    .filter(|entry| !cache.hidden.contains(&entry.game.id))
                    .filter(|entry| {
                        key.filters.accepts(
                            entry,
                            &cache.favorites,
                            &cache.stats,
                            &cache.collections,
                            &key.pending_installs,
                        )
                    })
                    .filter(|entry| key.query.is_empty() || entry.game.title.to_lowercase().contains(&key.query))
                    .cloned()
                    .collect();
                if visible.is_empty() {
                    rows.push(Row::Message("Sin resultados."));
                } else {
                    sort_entries(&mut visible, key.filters.sort, &cache.stats);
                    let shelves = shelves::build(
                        &visible,
                        &cache.collections,
                        &cache.favorites,
                        &cache.stats,
                        &key.pending_installs,
                    );
                    for shelf in shelves {
                        let collapsed = self.collapsed.contains(&shelf.id);
                        if shelf.collection_id.is_some() && shelf.collection_id == key.renaming {
                            rows.push(Row::Rename);
                        }
                        rows.push(Row::Head(ShelfHead {
                            installed: shelf.installed_count(&key.pending_installs),
                            total: shelf.entries.len(),
                            shelf_id: shelf.id.clone(),
                            title: shelf.title.clone(),
                            collection_id: shelf.collection_id.clone(),
                            dynamic: shelf.dynamic,
                            collapsed,
                        }));
                        if collapsed {
                            continue;
                        }
                        if shelf.entries.is_empty() {
                            rows.push(Row::Empty {
                                shelf: shelf.collection_id.clone(),
                            });
                        }
                        for entry in shelf.entries {
                            rows.push(Row::Item {
                                installed: is_installed(&entry.game, &key.pending_installs),
                                game: entry.game,
                                shelf: shelf.collection_id.clone(),
                            });
                        }
                    }
                    rows.push(Row::Spacer);
                }
            }
        }
        self.order = cache
            .collections
            .list
            .iter()
            .map(|collection| collection.id.clone())
            .collect();
        // `splice`, not `reset`: a reset scrolls the list back to the top
        // on every library batch or filter tick.
        self.list.splice(0..self.rows.len(), rows.len());
        self.rows = rows;
        self.rows_key = Some(key);
    }

    fn render_row(&self, index: usize, cx: &mut Context<Self>) -> AnyElement {
        let Some(row) = self.rows.get(index) else {
            return div().into_any_element();
        };
        let dims = |shelf: &Option<String>| shelf.is_some() && self.dragging_shelf.as_deref() == shelf.as_deref();
        match row {
            Row::Message(text) => div()
                .px(px(14.))
                .text_color(rgb(theme::TEXT_MUTED))
                .child(*text)
                .into_any_element(),
            Row::Error(err) => div().mx(px(14.)).child(theme::error(err.clone())).into_any_element(),
            Row::Rename => match &self.rename {
                Some((_, input, _)) => div()
                    .mx(px(14.))
                    .mt(px(10.))
                    .mb(px(2.))
                    .child(input.clone())
                    .into_any_element(),
                None => div().into_any_element(),
            },
            Row::Head(head) => {
                let dragging = dims(&head.collection_id);
                self.shelf_head(head, cx)
                    .when(dragging, |head| head.opacity(0.45))
                    .into_any_element()
            }
            Row::Item { game, installed, shelf } => {
                let selected = self.model.read(cx).selected.clone();
                self.item(game, *installed, selected.as_deref(), cx)
                    .when(dims(shelf), |item| item.opacity(0.45))
                    .into_any_element()
            }
            Row::Empty { shelf } => div()
                .mx(px(14.))
                .px_3()
                .py(px(10.))
                .border_1()
                .border_dashed()
                .border_color(rgb(theme::LINE))
                .rounded(px(theme::RADIUS))
                .text_color(rgb(theme::TEXT_DIM))
                .text_size(px(12.))
                .when(dims(shelf), |empty| empty.opacity(0.45))
                .child("Vacia")
                .into_any_element(),
            Row::Spacer => div().h_2().into_any_element(),
        }
    }

    fn shelf_head(&self, shelf: &ShelfHead, cx: &mut Context<Self>) -> Stateful<gpui::Div> {
        let shelf_id = shelf.shelf_id.clone();
        let is_collapsed = shelf.collapsed;
        // Games can be dropped only on a collection whose members are a
        // list, not a filter.
        let drop_collection = shelf.collection_id.clone().filter(|_| !shelf.dynamic);
        let reorder_id = shelf.collection_id.clone();
        let installed = shelf.installed;
        let total = shelf.total;
        let title: SharedString = shelf.title.clone().into();
        let order = &self.order;

        let mut head = div()
            .id(SharedString::from(format!("shelf-head-{shelf_id}")))
            .flex()
            .items_center()
            .gap(px(7.))
            .w_full()
            .px(px(14.))
            .pt(px(14.))
            .pb(px(6.))
            .font_family(theme::DISPLAY)
            .font_weight(FontWeight::SEMIBOLD)
            .text_size(px(10.5))
            .text_color(rgb(theme::TEXT_DIM))
            .cursor_pointer()
            .hover(|head| head.text_color(rgb(theme::TEXT_MUTED)))
            .when(drop_collection.is_some(), |head| {
                head.bg(rgb(theme::BG_PANEL)).shadow(vec![BoxShadow {
                    color: rgb(theme::ACCENT_BLUE).into(),
                    offset: point(px(0.), px(0.)),
                    blur_radius: px(0.),
                    spread_radius: px(1.),
                    inset: true,
                }])
            })
            .child(div().w(px(9.)).flex_none().child(if is_collapsed { "▸" } else { "▾" }))
            .child(div().flex_1().truncate().child(shelf.title.to_uppercase()))
            // The browser tags these too; a collection shouldn't look
            // hand-made in one pane and generated in the other.
            .when(shelf.dynamic, |head| head.child(auto_tag("AUTO")))
            // Steam's (installed/total) — how much of this shelf is
            // actually playable right now.
            .child(theme::mono().flex_none().child(format!("({installed}/{total})")))
            .on_click(cx.listener({
                let shelf_id = shelf_id.clone();
                move |this, _, _, cx| {
                    if !this.collapsed.insert(shelf_id.clone()) {
                        this.collapsed.remove(&shelf_id);
                    }
                    if let Some(id) = Self::profile(cx) {
                        data::save_collapsed_shelves(&id, &this.collapsed);
                    }
                    cx.notify();
                }
            }));

        if let Some(collection_id) = shelf.collection_id.clone() {
            let menu_title = shelf.title.clone();
            head = head.on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                    let target = ShelfMenuTarget {
                        collection_id: collection_id.clone(),
                        title: menu_title.clone(),
                        position: event.position,
                    };
                    this.model.update(cx, |model, cx| {
                        model.open_popup(LibraryPopup::ShelfMenu(target), cx);
                    });
                }),
            );
        }

        // The header doubles as the drag handle for reordering, the way
        // Steam's own collection rows do — only real collections can be
        // picked up this way, dynamic or not: unlike a game drop, a
        // reorder never touches which games are members of anything.
        if let Some(reorder_id) = reorder_id.clone() {
            let entity = cx.entity().downgrade();
            let target_id = reorder_id.clone();
            let order = order.to_vec();
            head = head
                .on_drag(
                    DraggedShelf {
                        id: reorder_id,
                        title: title.clone(),
                    },
                    move |dragged: &DraggedShelf, _, _, cx| {
                        let id = dragged.id.clone();
                        let _ = entity.update(cx, |this, cx| {
                            this.dragging_shelf = Some(id);
                            cx.notify();
                        });
                        cx.new(|_| DragGhost(dragged.title.clone()))
                    },
                )
                .drag_over::<DraggedShelf>({
                    let target_id = target_id.clone();
                    move |style, dragged, _, _| {
                        if dragged.id == target_id {
                            style
                        } else {
                            style.shadow(vec![BoxShadow {
                                color: rgb(theme::ACCENT_BLUE).into(),
                                offset: point(px(0.), px(-2.)),
                                blur_radius: px(0.),
                                spread_radius: px(0.),
                                inset: false,
                            }])
                        }
                    }
                })
                .on_drop(cx.listener(move |this, dragged: &DraggedShelf, _, cx| {
                    this.dragging_shelf = None;
                    let (Some(from), Some(to)) = (
                        order.iter().position(|id| *id == dragged.id),
                        order.iter().position(|id| *id == target_id),
                    ) else {
                        return;
                    };
                    let delta = to as i64 - from as i64;
                    if delta != 0
                        && let Some(profile_id) = Self::profile(cx)
                    {
                        data::move_collection(&profile_id, &dragged.id, delta);
                        super::actions::bump_organisation(cx);
                    }
                    cx.notify();
                }));
        }
        if let Some(collection_id) = drop_collection {
            head = head.on_drop(cx.listener(move |_, dragged: &DraggedGame, _, cx| {
                data::set_collection_member(&collection_id, &dragged.id, true);
                super::actions::bump_organisation(cx);
            }));
        }

        head
    }

    fn item(
        &self,
        game: &Game,
        installed: bool,
        selected: Option<&str>,
        cx: &mut Context<Self>,
    ) -> Stateful<gpui::Div> {
        let is_active = selected == Some(game.id.as_str());
        let is_marked = self.marked.contains(&game.id);
        let icon = match game.icon_url.as_deref() {
            Some(url) => img(crate::art::source(url))
                .size(px(26.))
                .flex_none()
                .rounded(px(theme::RADIUS_SM))
                .object_fit(ObjectFit::Cover)
                .bg(rgb(theme::BG_PANEL_LIGHT))
                .with_fallback(|| icon_fallback().into_any_element())
                .into_any_element(),
            None => icon_fallback().into_any_element(),
        };
        let click_id = game.id.clone();
        let menu_id = game.id.clone();
        let menu_title = game.title.clone();
        div()
            .id(SharedString::from(format!("sidebar-item-{}", game.id)))
            .flex()
            .items_center()
            .gap(px(10.))
            .px(px(14.))
            .py(px(7.))
            .border_l(px(3.))
            .border_color(gpui::transparent_black())
            .text_size(px(12.5))
            .text_color(rgb(theme::TEXT_PRIMARY))
            .cursor_pointer()
            .hover(|item| item.bg(rgb(theme::BG_PANEL_2)))
            .when(is_marked && !is_active, |item| {
                item.bg(rgb(theme::BG_PANEL_LIGHT))
                    .border_color(rgb(theme::ACCENT_GOLD))
            })
            .when(is_active, |item| {
                item.bg(rgb(theme::BG_PANEL_2))
                    .text_color(rgb(theme::TEXT_BRIGHT))
                    .border_color(rgb(theme::ACCENT_BLUE))
            })
            .child(icon)
            .child(div().min_w_0().truncate().child(game.title.clone()))
            .when(installed, |item| {
                item.child(
                    div()
                        .id(SharedString::from(format!("installed-dot-{}", game.id)))
                        .ml_auto()
                        .size(px(6.))
                        .flex_none()
                        .rounded_full()
                        .bg(rgb(theme::ACCENT_GREEN))
                        .tooltip(tooltip("Instalado")),
                )
            })
            .on_drag(
                DraggedGame {
                    id: game.id.clone(),
                    title: game.title.clone().into(),
                },
                |dragged: &DraggedGame, _, _, cx| cx.new(|_| DragGhost(dragged.title.clone())),
            )
            .on_click(cx.listener(move |this, event: &ClickEvent, _, cx| {
                // Ctrl-click extends a selection to act on in bulk; a plain
                // click is still just "show me this one".
                if event.modifiers().control {
                    if !this.marked.insert(click_id.clone()) {
                        this.marked.remove(&click_id);
                    }
                    cx.notify();
                    return;
                }
                this.marked.clear();
                let id = click_id.clone();
                this.model.update(cx, |model, cx| model.select(Some(id), cx));
            }))
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                    // Right-clicking a marked game targets the whole marked
                    // set; right-clicking anything else targets just it.
                    let (product_ids, title) = if this.marked.contains(&menu_id) && this.marked.len() > 1 {
                        (
                            this.marked.iter().cloned().collect(),
                            format!("{} juegos", this.marked.len()),
                        )
                    } else {
                        (vec![menu_id.clone()], menu_title.clone())
                    };
                    let target = MenuTarget {
                        product_ids,
                        title,
                        position: event.position,
                    };
                    this.model.update(cx, |model, cx| {
                        model.open_popup(LibraryPopup::GameMenu(target), cx);
                    });
                }),
            )
    }

    /// Drawn by the library page itself, over the detail pane: the panel
    /// starts where the sidebar ends, outside the sidebar's own bounds.
    pub fn filter_panel(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.panel_open {
            return None;
        }
        let profile_id = Self::profile(cx)?;
        self.refresh_cache(&profile_id, cx);
        let state = AppState::get(cx).clone();
        let pending_installs = pending_install_ids(&state.background.read(cx).installing);
        let current = self.model.read(cx).filters.clone();
        let games = &state.library.read(cx).games;
        let query = self.search.read(cx).text().to_lowercase();
        let kind = self.kind;
        let cache = &self.cache;
        // The denominator for the result line: everything the chosen
        // product type covers, before any filter narrows it — with the
        // same exclusions the list applies, or "X de Y" counts rows
        // nothing can ever show.
        let in_kind: Vec<&LibraryEntry> = games
            .iter()
            .filter(|entry| kind.accepts(entry.kind))
            .filter(|entry| entry.parent_id.is_none())
            .filter(|entry| belongs_in_the_library(entry))
            .filter(|entry| !cache.hidden.contains(&entry.game.id))
            .collect();
        let matched = games
            .iter()
            .filter(|entry| kind.accepts(entry.kind))
            .filter(|entry| !cache.hidden.contains(&entry.game.id))
            .filter(|entry| {
                current.accepts(
                    entry,
                    &cache.favorites,
                    &cache.stats,
                    &cache.collections,
                    &pending_installs,
                )
            })
            .filter(|entry| query.is_empty() || entry.game.title.to_lowercase().contains(&query))
            .count();
        let total = in_kind.len();
        let storefronts = available_storefronts(&in_kind);
        let tallies = count_options(
            &in_kind,
            &cache.favorites,
            &cache.stats,
            &cache.collections,
            &pending_installs,
        );
        // Drawn from the whole library, not the filtered view — otherwise
        // picking a genre would empty the very picker it was chosen from.
        let genres: Vec<String> = games
            .iter()
            .flat_map(|entry| entry.game.genres.iter().cloned())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        // Manual collections only — a dynamic one is itself a filter.
        let collections: Vec<(String, String)> = cache
            .collections
            .list
            .iter()
            .filter(|collection| collection.filter.is_none())
            .map(|collection| (collection.id.clone(), collection.name.clone()))
            .collect();
        let accounts = cache.accounts.clone();
        let active = current.active_count();

        let entity = cx.entity();
        let check_row =
            |id: SharedString, label: String, tally: usize, checked: bool, toggle: Box<dyn Fn(&mut Filters)>| {
                let entity = entity.clone();
                div()
                    .id(id)
                    .flex()
                    .items_center()
                    .gap(px(9.))
                    .py_1()
                    .text_size(px(12.5))
                    .cursor_pointer()
                    .hover(|row| row.text_color(rgb(theme::TEXT_BRIGHT)))
                    .on_click(move |_, _, cx| entity.update(cx, |this, cx| this.update_filters(cx, |f| toggle(f))))
                    .child(checkbox(checked))
                    .child(div().min_w_0().truncate().child(label))
                    .child(
                        theme::mono()
                            .ml_auto()
                            .text_size(px(10.))
                            .text_color(rgb(theme::TEXT_DIM))
                            .child(tally.to_string()),
                    )
            };
        let group = |title: &str, note: &str| {
            div().flex().flex_col().child(group_title(title)).child(
                div()
                    .mb_2()
                    .text_size(px(10.5))
                    .line_height(gpui::relative(1.45))
                    .text_color(rgb(theme::TEXT_DIM))
                    .child(note.to_owned()),
            )
        };

        let states = group("Estado", "Propiedades del mismo juego: marcar dos acota.").children(
            [
                (PlayState::Installed, "Instalados"),
                (PlayState::Played, "Jugados"),
                (PlayState::Unplayed, "Sin jugar"),
                (PlayState::Favorite, "Favoritos"),
            ]
            .into_iter()
            .map(|(state, label)| {
                check_row(
                    format!("check-state-{label}").into(),
                    label.to_owned(),
                    tallies.states.get(&state).copied().unwrap_or(0),
                    current.states.contains(&state),
                    Box::new(move |filters| {
                        if !filters.states.insert(state) {
                            filters.states.remove(&state);
                        }
                    }),
                )
            }),
        );
        let achievements = group("Logros", "Segun lo que Decanter tiene cacheado.").children(
            [
                (AchievementState::Complete, "Completados"),
                (AchievementState::InProgress, "En progreso"),
                (AchievementState::NotStarted, "Sin empezar"),
                (AchievementState::Unmeasured, "Sin logros"),
            ]
            .into_iter()
            .map(|(achievement, label)| {
                check_row(
                    format!("check-achievement-{label}").into(),
                    label.to_owned(),
                    tallies.achievements.get(&achievement).copied().unwrap_or(0),
                    current.achievements.contains(&achievement),
                    Box::new(move |filters| {
                        if !filters.achievements.insert(achievement) {
                            filters.achievements.remove(&achievement);
                        }
                    }),
                )
            }),
        );
        let sort_options = [
            ("title", "Nombre"),
            ("played", "Mas jugado"),
            ("recent", "Jugado recientemente"),
            ("completion", "Logros"),
            ("release", "Lanzamiento"),
            ("size", "Tamano en disco"),
            ("rating", "Calificacion"),
            ("added", "Fecha agregada"),
        ]
        .map(|(value, label)| (SharedString::from(value), SharedString::from(label)));
        let sort_entity = entity.clone();
        let mut order = group("Orden de la lista", "Cada estanteria de portadas lleva el suyo.").child(
            Select::new("filter-sort", sort_options, current.sort.value())
                .style(SelectStyle::Group)
                .full_width()
                .on_change(move |value, _, cx| {
                    let sort = Sort::from_value(value);
                    sort_entity.update(cx, |this, cx| this.update_filters(cx, |filters| filters.sort = sort));
                }),
        );
        if !collections.is_empty() {
            let options = std::iter::once((SharedString::default(), SharedString::from("Cualquier coleccion"))).chain(
                collections.iter().map(|(id, name)| {
                    let count = tallies.collections.get(id).copied().unwrap_or(0);
                    (
                        SharedString::from(id.clone()),
                        SharedString::from(format!("{name} ({count})")),
                    )
                }),
            );
            let collection_entity = entity.clone();
            order = order.child(div().mt_4().child(group_title("Coleccion"))).child(
                Select::new(
                    "filter-collection",
                    options,
                    current.collection.clone().unwrap_or_default(),
                )
                .style(SelectStyle::Group)
                .full_width()
                .on_change(move |value, _, cx| {
                    let value = value.to_string();
                    collection_entity.update(cx, |this, cx| {
                        this.update_filters(cx, |filters| filters.collection = (!value.is_empty()).then_some(value));
                    });
                }),
            );
        }

        let mut grid = div()
            .grid()
            .grid_cols(3)
            .gap_x(px(26.))
            .gap_y(px(22.))
            .px_6()
            .pt(px(18.))
            .pb(px(22.))
            .child(states)
            .child(achievements)
            .child(order);
        if accounts.len() > 1 {
            grid = grid.child(
                group(
                    "Cuenta",
                    "Este perfil tiene mas de una — marcar dos amplia el resultado.",
                )
                .children(accounts.into_iter().map(|(id, name)| {
                    let toggle_id = id.clone();
                    check_row(
                        format!("check-account-{id}").into(),
                        name,
                        tallies.accounts.get(&id).copied().unwrap_or(0),
                        current.accounts.contains(&id),
                        Box::new(move |filters| {
                            if !filters.accounts.insert(toggle_id.clone()) {
                                filters.accounts.remove(&toggle_id);
                            }
                        }),
                    )
                })),
            );
        }
        if storefronts.len() > 1 {
            grid = grid.child(
                group("Tienda", "Alternativas: marcar dos amplia el resultado.").children(storefronts.into_iter().map(
                    |storefront| {
                        check_row(
                            format!("check-store-{}", storefront.as_str_name()).into(),
                            storefront_label(storefront).to_owned(),
                            tallies.storefronts.get(&storefront).copied().unwrap_or(0),
                            current.storefronts.contains(&storefront),
                            Box::new(move |filters| {
                                if !filters.storefronts.insert(storefront) {
                                    filters.storefronts.remove(&storefront);
                                }
                            }),
                        )
                    },
                )),
            );
        }

        // `repeat(auto-fill, minmax(160px, 1fr))`, worked out from the
        // panel's width: GPUI's grid only takes a fixed column count.
        let panel_width = (window.viewport_size().width.as_f32() - SIDEBAR_WIDTH).min(720.);
        let genre_columns = (((panel_width - 48. + 26.) / (160. + 26.)).floor() as u16).max(1);
        let genre_body = if genres.is_empty() {
            theme::muted("Sin generos todavia.").into_any_element()
        } else {
            div()
                .grid()
                .grid_cols(genre_columns)
                .gap_x(px(26.))
                .children(genres.into_iter().map(|name| {
                    let toggle_name = name.clone();
                    check_row(
                        format!("check-genre-{name}").into(),
                        name.clone(),
                        tallies.genres.get(&name).copied().unwrap_or(0),
                        current.genres.contains(&name),
                        Box::new(move |filters| {
                            if !filters.genres.insert(toggle_name.clone()) {
                                filters.genres.remove(&toggle_name);
                            }
                        }),
                    )
                }))
                .into_any_element()
        };
        grid = grid.child(
            group("Genero", "Alternativas: marcar dos amplia el resultado.")
                .col_span_full()
                .child(genre_body),
        );

        let head = div()
            .flex()
            .items_baseline()
            .gap_3()
            .px_6()
            .pt(px(20.))
            .pb_1()
            .child(
                theme::display(22.)
                    .flex_1()
                    .text_color(rgb(theme::TEXT_BRIGHT))
                    .child("FILTROS DE BIBLIOTECA"),
            )
            .when(!current.is_default(), |head| {
                head.child(
                    link_button("filters-clear")
                        .child("Limpiar filtros")
                        .on_click(cx.listener(|this, _, _, cx| {
                            // Sort survives a reset: it's a view preference, not
                            // a constraint the user is clearing.
                            this.update_filters(cx, |filters| {
                                let sort = filters.sort;
                                *filters = Filters {
                                    sort,
                                    ..Filters::default()
                                };
                            });
                        })),
                )
            })
            .child(
                link_button("filters-close")
                    .child("✕")
                    .on_click(cx.listener(|this, _, window, cx| this.set_panel(false, window, cx))),
            );

        // Naming is the only step this adds: the filter itself is already
        // whatever the panel above is set to.
        let save = if self.saving {
            div().w(px(220.)).child(self.save_name.clone()).into_any_element()
        } else {
            div()
                .id("save-dynamic")
                .px_4()
                .py(px(9.))
                .rounded(px(theme::RADIUS_SM))
                .bg(rgb(theme::ACCENT_BLUE))
                .hover(|button| button.bg(rgb(theme::ACCENT_BLUE_HOVER)))
                .text_color(rgb(theme::TEXT_ON_BLUE))
                .text_size(px(12.5))
                .font_weight(FontWeight::BOLD)
                .cursor_pointer()
                .on_click(cx.listener(|this, _, window, cx| {
                    this.saving = true;
                    window.focus(&this.save_name.focus_handle(cx), cx);
                    cx.notify();
                }))
                .child("Guardar como coleccion dinamica")
                .into_any_element()
        };
        let foot = div()
            .flex()
            .items_center()
            .gap(px(14.))
            .px_6()
            .pt(px(14.))
            .pb(px(20.))
            .border_t_1()
            .border_color(rgb(theme::LINE))
            .child(
                div()
                    .flex_1()
                    .text_size(px(12.))
                    .text_color(rgb(theme::TEXT_MUTED))
                    .child(if active == 0 {
                        format!("{total} productos")
                    } else {
                        format!("{matched} de {total} coinciden · {active} filtros")
                    }),
            )
            .child(save);

        let panel = div()
            .id("filter-panel")
            .track_focus(&self.panel_focus)
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                if event.keystroke.key == "escape" {
                    this.set_panel(false, window, cx);
                }
            }))
            .absolute()
            .top_0()
            .left(px(SIDEBAR_WIDTH))
            .w(px(panel_width))
            .max_h_full()
            .overflow_y_scroll()
            .occlude()
            .bg(rgb(theme::BG_PANEL_2))
            .border_r_1()
            .border_b_1()
            .border_color(rgb(theme::LINE))
            .shadow(vec![BoxShadow {
                color: gpui::black().opacity(0.38),
                offset: point(px(18.), px(0.)),
                blur_radius: px(40.),
                spread_radius: px(0.),
                inset: false,
            }])
            .child(head)
            .child(grid)
            .child(foot);
        // A full-bleed click target behind the panel, so clicking anywhere
        // outside closes it, the same as every other popup in this view.
        let backdrop = div()
            .id("filter-backdrop")
            .absolute()
            .inset_0()
            .occlude()
            .on_click(cx.listener(|this, _, window, cx| this.set_panel(false, window, cx)));
        Some(
            div()
                .absolute()
                .inset_0()
                .child(backdrop)
                .child(panel)
                .into_any_element(),
        )
    }
}

fn icon_fallback() -> gpui::Div {
    div()
        .size(px(26.))
        .flex_none()
        .rounded(px(theme::RADIUS_SM))
        .bg(rgb(theme::BG_PANEL_LIGHT))
}

fn group_title(text: &str) -> gpui::Div {
    theme::display(11.)
        .mb_1()
        .text_color(rgb(theme::ACCENT_BLUE))
        .child(text.to_uppercase())
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

/// `.link-btn`: bare muted text, blue on a panel fill when hovered.
fn link_button(id: &'static str) -> Stateful<gpui::Div> {
    div()
        .id(id)
        .px(px(5.))
        .py(px(3.))
        .rounded(px(theme::RADIUS_SM))
        .text_size(px(12.))
        .text_color(rgb(theme::TEXT_MUTED))
        .cursor_pointer()
        .hover(|button| button.text_color(rgb(theme::ACCENT_BLUE)).bg(rgb(theme::BG_PANEL)))
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
            .child(self.header(active_filters, cx))
            .child(
                list(
                    self.list.clone(),
                    cx.processor(|this, index, _, cx| this.render_row(index, cx)),
                )
                .flex_1()
                .min_h_0(),
            )
    }
}
