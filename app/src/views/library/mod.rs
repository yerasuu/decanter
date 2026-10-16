//! The single "games" section: a Steam-like sidebar of the profile's GOG
//! library on the left, and on the right either the shelf browser or the
//! selected game's detail.
//!
//! The pieces are separate views sharing one [`LibraryModel`] — what's
//! selected, which popup is open, the filters — so a change in one (a
//! right-click in the browser, a click in the sidebar) shows in the others.

mod actions;
mod browser;
mod context_menu;
mod data;
mod detail;
mod filters;
mod fmt;
pub mod install;
pub mod properties;
mod shelf_popups;
mod shelves;
mod sidebar;

use gpui::{Context, Entity, Pixels, Point, StyleRefinement, Task, Window, div, prelude::*, px, relative, rgb};
use rust_i18n::t;

use browser::BrowserView;
use context_menu::MenuTarget;
use data::PollStatus;
use detail::DetailView;
use shelf_popups::ShelfMenuTarget;
use sidebar::SidebarView;

use crate::state::{AppState, LibraryState};
use crate::theme;
use crate::widgets::progress_bar;

/// The one menu-like popup the whole view can have open at a time — a
/// game's context menu, a shelf's, or the "new collection" popup. Shared
/// between the sidebar and the browser (both open a `GameMenu`) so that
/// opening any of these anywhere closes whichever other one was open,
/// instead of the two stacking — the way a right-click anywhere in a real
/// app replaces whatever menu was up.
///
/// The filter panel is deliberately not one of these: it's a persistent
/// drawer with its own explicit close, not a transient menu.
#[derive(Clone)]
enum LibraryPopup {
    GameMenu(MenuTarget),
    ShelfMenu(ShelfMenuTarget),
    NewCollection(Point<Pixels>),
}

struct LibraryModel {
    pub(crate) selected: Option<String>,
    pub(crate) popup: Option<LibraryPopup>,
    /// Owned here, not by the sidebar, so the browser sees the same "solo
    /// instalados" state: that one condition narrows the whole library
    /// view, unlike the rest of the filters, which stay sidebar-only.
    pub(crate) filters: filters::Filters,
    /// A collection whose name is being edited inline in the sidebar, as
    /// `(id, current name)` — started from the shelf menu's "Renombrar".
    pub(crate) renaming: Option<(String, String)>,
    pub(crate) poll_status: PollStatus,
    /// The selected game's cached achievements, re-read after each poll.
    pub(crate) achievements: Option<Result<Vec<data::Achievement>, String>>,
    poll: Option<Task<()>>,
}

impl LibraryModel {
    fn new(cx: &mut Context<Self>) -> Self {
        let session = AppState::get(cx).session.clone();
        let mut library_home = session.read(cx).library_home;
        // Clicking "Biblioteca" in the topbar always drops back to the
        // shelf browser, even while already there.
        cx.observe(&session, move |this, session, cx| {
            let home = session.read(cx).library_home;
            if home != library_home {
                library_home = home;
                this.selected = None;
                this.popup = None;
                this.poll = None;
                this.poll_status = PollStatus::Idle;
                this.achievements = None;
                cx.notify();
            }
        })
        .detach();
        Self {
            selected: None,
            popup: None,
            filters: filters::Filters::default(),
            renaming: None,
            poll_status: PollStatus::Idle,
            achievements: None,
            poll: None,
        }
    }

    pub fn open_popup(&mut self, popup: LibraryPopup, cx: &mut Context<Self>) {
        self.popup = Some(popup);
        cx.notify();
    }

    pub fn close_popup(&mut self, cx: &mut Context<Self>) {
        self.popup = None;
        cx.notify();
    }

    /// Selects a game and polls GOG for its achievements right away — the
    /// cache still shows instantly, this just refreshes it. The status is
    /// tracked, not just the resulting cache, so the empty case can say
    /// *why* it's empty.
    ///
    /// Keyed off the game's own account, not the active profile: with
    /// several accounts merged into one library, achievements belong to
    /// whichever account's copy is actually on screen.
    pub fn select(&mut self, product_id: Option<String>, cx: &mut Context<Self>) {
        self.selected = product_id;
        // An external game has no achievements anywhere to poll.
        let account_id = self.selected.as_deref().and_then(|id| {
            AppState::get(cx)
                .library
                .read(cx)
                .games
                .iter()
                .find(|entry| entry.game.id == id && entry.game.storefront == decanter_core::Storefront::Gog)
                .map(|entry| entry.game.account_id.clone())
        });
        match (account_id, self.selected.clone()) {
            (Some(account_id), Some(product_id)) => {
                self.poll_status = PollStatus::Polling;
                self.achievements = Some(
                    data::load_achievements(account_id.clone(), product_id.clone()).map_err(|err| err.to_string()),
                );
                self.poll = Some(cx.spawn(async move |this, cx| {
                    let result = data::poll_achievements(account_id.clone(), product_id.clone()).await;
                    let _ = this.update(cx, |this, cx| {
                        this.poll_status = match result {
                            Ok(()) => PollStatus::Done,
                            Err(err) => PollStatus::Failed(err),
                        };
                        this.achievements =
                            Some(data::load_achievements(account_id, product_id).map_err(|err| err.to_string()));
                        cx.notify();
                    });
                }));
            }
            _ => {
                self.poll = None;
                self.poll_status = PollStatus::Idle;
                self.achievements = None;
            }
        }
        cx.notify();
    }
}

pub struct LibraryView {
    model: Entity<LibraryModel>,
    sidebar: Entity<SidebarView>,
    browser: Entity<BrowserView>,
    detail: Entity<DetailView>,
}

impl LibraryView {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let state = AppState::get(cx).clone();
        cx.observe(&state.library, |_, _, cx| cx.notify()).detach();
        cx.observe(&state.session, |_, _, cx| cx.notify()).detach();
        let model = cx.new(LibraryModel::new);
        cx.observe(&model, |_, _, cx| cx.notify()).detach();
        let sidebar = cx.new(|cx| SidebarView::new(model.clone(), window, cx));
        // The filter panel is the sidebar's, but drawn here.
        cx.observe(&sidebar, |_, _, cx| cx.notify()).detach();
        Self {
            sidebar,
            browser: cx.new(|cx| BrowserView::new(model.clone(), window, cx)),
            detail: cx.new(|cx| DetailView::new(model.clone(), window, cx)),
            model,
        }
    }
}

impl Render for LibraryView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let state = AppState::get(cx).clone();
        let (loading_state, count, enriching) = {
            let library = state.library.read(cx);
            (
                library.state == LibraryState::Loading,
                library.games.len(),
                library.enrich_progress,
            )
        };
        // Only reached for an account with nothing cached yet — a returning
        // account paints its cached list immediately. This stays up until
        // that first-ever sync is fully done, cheaper than letting the
        // sidebar and detail mount on a half-resolved library and watch
        // fields change under them as the fetch lands.
        if loading_state {
            return loading(count, enriching).into_any_element();
        }
        let has_profile = state.session.read(cx).active_profile.is_some();
        let selected = self
            .model
            .read(cx)
            .selected
            .as_deref()
            .is_some_and(|id| state.library.read(cx).games.iter().any(|entry| entry.game.id == id));
        let pane = match (selected, has_profile) {
            (true, true) => div()
                .id("library-detail")
                .size_full()
                .overflow_y_scroll()
                .child(div().pb(px(60.)).child(self.detail.clone()))
                .into_any_element(),
            // Nothing selected: the shelf browser stands in for an empty
            // pane, so the default view is the library rather than a
            // prompt to go find it.
            (false, true) => self
                .browser
                .clone()
                .cached(StyleRefinement::default().size_full())
                .into_any_element(),
            (_, false) => div()
                .m_8()
                .child(theme::empty(t!("common.pick_profile")))
                .into_any_element(),
        };
        let filter_panel = self.sidebar.update(cx, |sidebar, cx| sidebar.filter_panel(window, cx));
        let popup = context_menu::render_popup(&self.model, window, cx);

        div()
            .size_full()
            .relative()
            .flex()
            .overflow_hidden()
            .child(
                self.sidebar
                    .clone()
                    .cached(StyleRefinement::default().w(px(292.)).h_full().flex_none()),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .flex()
                    .flex_col()
                    .children(offline_notice(&state.library.read(cx).offline_folders))
                    .child(div().flex_1().min_h_0().child(pane)),
            )
            .children(filter_panel)
            .children(popup)
            .into_any_element()
    }
}

/// Says which library folders aren't plugged in: their games read as not
/// installed until they are, and installing one again elsewhere would make
/// a second copy.
fn offline_notice(offline: &[std::path::PathBuf]) -> Option<gpui::Div> {
    if offline.is_empty() {
        return None;
    }
    let folders = offline
        .iter()
        .map(|path| path.display().to_string())
        .collect::<Vec<_>>()
        .join(", ");
    Some(
        div()
            .flex_none()
            .px_6()
            .py_2()
            .bg(rgb(theme::BG_PANEL_2))
            .border_b_1()
            .border_color(rgb(theme::LINE))
            .text_size(px(12.5))
            .text_color(rgb(theme::TEXT_MUTED))
            .child(t!("library.offline_folders", folders = folders)),
    )
}

/// Steam's Propiedades for one game, in a window of its own.
fn open_properties(game: &decanter_core::Game, cx: &mut gpui::App) {
    properties::open_properties_window(game.clone(), game.account_id.clone(), cx);
}

/// The splash a first sync sits behind, the way Steam's own "Syncing..."
/// screen does — full-width rather than tucked into the sidebar list.
///
/// `enriching` is pass two's "N of M" once pass one has a known total —
/// before that the bar stays indeterminate, same as an install whose total
/// size isn't known yet.
fn loading(count: usize, enriching: Option<(usize, usize)>) -> impl IntoElement {
    let fraction = enriching.map(|(done, total)| done as f64 / total.max(1) as f64);
    let text = match (count, enriching) {
        (0, _) => t!("library.loading.searching"),
        (count, Some((done, total))) => t!("library.loading.enriching", done = done, total = total, count = count),
        (1, None) => t!("library.loading.found.one", count = 1),
        (count, None) => t!("library.loading.found.other", count = count),
    };
    div().size_full().flex().items_center().justify_center().child(
        div()
            .w(px(360.))
            .max_w(relative(0.8))
            .flex()
            .flex_col()
            .items_center()
            .text_center()
            .child(
                theme::display(15.)
                    .text_color(rgb(theme::TEXT_MUTED))
                    .mt_8()
                    .mb(px(20.))
                    .child(t!("library.loading.title")),
            )
            .child(
                div()
                    .w_full()
                    .mb(px(14.))
                    .child(progress_bar("library-loading", fraction, 6.)),
            )
            .child(theme::muted(text)),
    )
}
