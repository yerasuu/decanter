use std::collections::HashSet;

use decanter_core::Game;
use decanter_gog::LibraryEntry;
use gpui::{
    AnyElement, App, BoxShadow, ClickEvent, Context, FontWeight, MouseButton, MouseDownEvent, ObjectFit, SharedString,
    Stateful, Window, div, img, point, prelude::*, px, rgb,
};
use rust_i18n::t;

use super::{CacheKey, SidebarView};
use crate::state::{AppState, LibraryState};
use crate::theme;
use crate::views::library::context_menu::{MenuTarget, auto_tag};
use crate::views::library::filters::{Filters, KindSelection, is_installed, pending_install_ids, sort_entries};
use crate::views::library::shelf_popups::ShelfMenuTarget;
use crate::views::library::{LibraryPopup, actions, data, shelves};
use crate::widgets::{running_mark, sidebar_title};

/// Everything the list's rows are derived from; the rows are rebuilt only
/// when one of these moves, not on every render.
#[derive(PartialEq)]
pub(super) struct RowsKey {
    revision: u64,
    kind: KindSelection,
    query: String,
    filters: Filters,
    cache: Option<CacheKey>,
    pending_installs: HashSet<String>,
    collapsed: HashSet<String>,
    renaming: Option<String>,
    state: LibraryState,
    /// The rows hold translated text (messages, shelf titles).
    locale: String,
}

pub(super) struct ShelfHead {
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
pub(super) enum Row {
    Message(SharedString),
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
    storefront: decanter_core::Storefront,
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

impl SidebarView {
    pub(super) fn rebuild_rows(&mut self, cx: &App) {
        let key = self.rows_key(cx);
        if self.rows_key.as_ref() == Some(&key) {
            return;
        }
        let rows = self.rows_for(&key, cx);
        self.order = self
            .cache
            .collections
            .list
            .iter()
            .map(|collection| collection.id.clone())
            .collect();
        // `splice`, not `reset`: a reset scrolls the list back to the top
        // on every library batch or filter tick.
        self.list.splice(0..self.rows.len(), rows.len());
        // Spliced-in rows count as zero height until drawn, which sized the
        // scrollbar thumb for the few rows seen so far. A game row's height
        // stands in for them; drawn rows keep their measured one.
        let _ = self.list.clone().with_uniform_item_height(px(40.));
        self.rows = rows;
        self.rows_key = Some(key);
    }

    fn rows_key(&self, cx: &App) -> RowsKey {
        let state = AppState::get(cx);
        let library = state.library.read(cx);
        RowsKey {
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
            locale: rust_i18n::locale().to_string(),
        }
    }

    fn rows_for(&self, key: &RowsKey, cx: &App) -> Vec<Row> {
        let library = AppState::get(cx).library.read(cx);
        let cache = &self.cache;
        let mut rows = Vec::new();
        match &key.state {
            LibraryState::Idle => rows.push(Row::Message(t!("common.pick_profile").into())),
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
                    rows.push(Row::Message(t!("library.sidebar.no_results").into()));
                } else {
                    sort_entries(&mut visible, key.filters.sort, &cache.stats);
                    self.push_shelf_rows(&mut rows, &visible, key);
                    rows.push(Row::Spacer);
                }
            }
        }
        rows
    }

    fn push_shelf_rows(&self, rows: &mut Vec<Row>, visible: &[LibraryEntry], key: &RowsKey) {
        let cache = &self.cache;
        let shelves = shelves::build(
            visible,
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
    }

    pub(super) fn render_row(&self, index: usize, cx: &mut Context<Self>) -> AnyElement {
        let Some(row) = self.rows.get(index) else {
            return div().into_any_element();
        };
        let dims = |shelf: &Option<String>| shelf.is_some() && self.dragging_shelf.as_deref() == shelf.as_deref();
        match row {
            Row::Message(text) => div()
                .px(px(14.))
                .text_color(rgb(theme::TEXT_MUTED))
                .child(text.clone())
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
                .child(t!("library.sidebar.empty_shelf"))
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
        let installed = shelf.installed;
        let total = shelf.total;

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
                let shelf_id = shelf_id;
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
            head = Self::with_shelf_menu(head, collection_id, shelf.title.clone(), cx);
        }

        // The header doubles as the drag handle for reordering, the way
        // Steam's own collection rows do — only real collections can be
        // picked up this way, dynamic or not: unlike a game drop, a
        // reorder never touches which games are members of anything.
        if let Some(reorder_id) = shelf.collection_id.clone() {
            head = self.with_shelf_reorder(head, reorder_id, shelf.title.clone().into(), cx);
        }
        if let Some(collection_id) = drop_collection {
            head = head.on_drop(cx.listener(move |_, dragged: &DraggedGame, _, cx| {
                data::set_collection_member(&collection_id, dragged.storefront, &dragged.id, true);
                actions::bump_organisation(cx);
            }));
        }

        head
    }

    fn with_shelf_menu(
        head: Stateful<gpui::Div>,
        collection_id: String,
        menu_title: String,
        cx: &Context<Self>,
    ) -> Stateful<gpui::Div> {
        head.on_mouse_down(
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
        )
    }

    fn with_shelf_reorder(
        &self,
        head: Stateful<gpui::Div>,
        reorder_id: String,
        title: SharedString,
        cx: &Context<Self>,
    ) -> Stateful<gpui::Div> {
        let entity = cx.entity().downgrade();
        let target_id = reorder_id.clone();
        let order = self.order.to_vec();
        head.on_drag(
            DraggedShelf { id: reorder_id, title },
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
                actions::bump_organisation(cx);
            }
            cx.notify();
        }))
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
        // Read here rather than baked into the cached rows, so a game starting or stopping
        // shows without rebuilding them.
        let running = AppState::get(cx).background.read(cx).running.contains(&game.id);
        let click_id = game.id.clone();
        let menu_id = game.id.clone();
        let menu_title = game.title.clone();
        div()
            .id(SharedString::from(format!("sidebar-item-{}", game.id)))
            // Otherwise the row is as wide as its title, which ran a long
            // one past the panel's edge instead of truncating it.
            .w_full()
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
            .when(!installed && !running, |item| item.text_color(rgb(theme::TEXT_MUTED)))
            .when(is_marked && !is_active, |item| {
                item.bg(rgb(theme::BG_PANEL_LIGHT))
                    .border_color(rgb(theme::ACCENT_GOLD))
            })
            .when(is_active, |item| {
                item.bg(rgb(theme::BG_PANEL_2))
                    .text_color(rgb(theme::TEXT_BRIGHT))
                    .border_color(rgb(theme::ACCENT_BLUE))
            })
            .child(item_icon(game))
            .child(sidebar_title(game.title.clone(), running))
            .when(running, |item| {
                item.child(running_mark(format!("running-mark-{}", game.id)).ml_auto())
            })
            .on_drag(
                DraggedGame {
                    id: game.id.clone(),
                    storefront: game.storefront,
                    title: game.title.clone().into(),
                },
                |dragged: &DraggedGame, _, _, cx| cx.new(|_| DragGhost(dragged.title.clone())),
            )
            .on_click(cx.listener(move |this, event: &ClickEvent, _, cx| this.click_item(&click_id, event, cx)))
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                    this.open_game_menu(&menu_id, &menu_title, event, cx);
                }),
            )
    }

    fn click_item(&mut self, id: &str, event: &ClickEvent, cx: &mut Context<Self>) {
        // Ctrl-click extends a selection to act on in bulk; a plain
        // click is still just "show me this one".
        if event.modifiers().control {
            if !self.marked.insert(id.to_owned()) {
                self.marked.remove(id);
            }
            cx.notify();
            return;
        }
        self.marked.clear();
        let id = id.to_owned();
        self.model.update(cx, |model, cx| model.select(Some(id), cx));
    }

    fn open_game_menu(&self, menu_id: &str, menu_title: &str, event: &MouseDownEvent, cx: &mut Context<Self>) {
        // Right-clicking a marked game targets the whole marked
        // set; right-clicking anything else targets just it.
        let (product_ids, title) = if self.marked.contains(menu_id) && self.marked.len() > 1 {
            (
                self.marked.iter().cloned().collect(),
                t!("library.menu.games", count = self.marked.len()).into_owned(),
            )
        } else {
            (vec![menu_id.to_owned()], menu_title.to_owned())
        };
        let target = MenuTarget {
            product_ids,
            title,
            position: event.position,
        };
        self.model.update(cx, |model, cx| {
            model.open_popup(LibraryPopup::GameMenu(target), cx);
        });
    }
}

fn item_icon(game: &Game) -> AnyElement {
    match game.icon_url.as_deref() {
        Some(url) => img(crate::art::source(url, gpui::size(26., 26.)))
            .size(px(26.))
            .flex_none()
            .rounded(px(theme::RADIUS_SM))
            .object_fit(ObjectFit::Cover)
            .bg(rgb(theme::BG_PANEL_LIGHT))
            .with_fallback(|| icon_fallback().into_any_element())
            .into_any_element(),
        None => icon_fallback().into_any_element(),
    }
}

fn icon_fallback() -> gpui::Div {
    div()
        .size(px(26.))
        .flex_none()
        .rounded(px(theme::RADIUS_SM))
        .bg(rgb(theme::BG_PANEL_LIGHT))
}
