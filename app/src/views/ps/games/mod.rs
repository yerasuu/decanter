//! Juegos: the profile's dumps in a sidebar, and beside it either the
//! cover browser or the selected game — the same shape as Biblioteca.

mod browser;
mod context_menu;
mod detail;
mod extras;
mod gamepad;
mod graphics;
mod imports;
mod keyboard;
mod mods;
mod online;
mod runs;
mod title_settings;

use decanter_ps::bbport::{Compat, CompatState, IniScope};
use decanter_ps::library::{Game, Platform};
use decanter_ps::status::{Entry, Status};
use std::borrow::Cow;
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::rc::Rc;

use gpui::{
    AnyElement, Context, Entity, FocusHandle, KeyDownEvent, MouseButton, MouseDownEvent, ObjectFit, Pixels, Point,
    ScrollStrategy, SharedString, StyleRefinement, UniformListScrollHandle, Window, div, img, point, prelude::*, px,
    rgb, uniform_list,
};
use rust_i18n::t;

use crate::i18n::Text;
use crate::state::{AppState, JobKey, Ps};
use crate::theme;
use crate::widgets::nav::{self, Step};
use crate::widgets::{ButtonKind, InputEvent, TextInput, button, running_mark, sidebar_title, tooltip};
use browser::BrowserView;
use context_menu::GameMenu;
use detail::DetailView;
pub(super) use gamepad::GamepadView;
pub(super) use graphics::GraphicsView;
pub(super) use keyboard::{Controls, KeyboardView};

const SIDEBAR_WIDTH: f32 = 292.;
const ICON: f32 = 26.;
/// One height for headers and games alike, so the list can be uniform.
const COMPATIBLE_GAMES_URL: &str = "https://github.com/yerasuu/psport/blob/develop/docs/en/GAMES.md";
const ROW: f32 = 40.;

#[derive(Clone, Copy)]
enum Row {
    Header(Platform, usize),
    Game(usize),
}

pub struct GamesView {
    ps: Entity<Ps>,
    search: Entity<TextInput>,
    detail: Entity<DetailView>,
    browser: Entity<BrowserView>,
    menu: Option<GameMenu>,
    /// Holds the keyboard inside the open menu.
    menu_focus: FocusHandle,
    /// What had the focus before the menu took it, given back when it closes.
    menu_restore: Option<FocusHandle>,
    list: UniformListScrollHandle,
    /// One per sidebar row, kept here since the list lays out only the rows in view and an arrow
    /// has to focus the next one before it exists.
    row_focus: RefCell<HashMap<PathBuf, FocusHandle>>,
}

impl GamesView {
    pub fn new(ps: Entity<Ps>, cx: &mut Context<Self>) -> Self {
        cx.observe(&ps, |_, _, cx| cx.notify()).detach();
        let detail = cx.new(|cx| DetailView::new(ps.clone(), cx));
        cx.observe(&detail, |_, _, cx| cx.notify()).detach();
        let games = cx.entity().downgrade();
        let browser = cx.new(|cx| BrowserView::new(ps.clone(), detail.clone(), games, cx));
        let search = cx.new(|cx| TextInput::new(Text::new("ps.games.search"), cx));
        cx.subscribe(&search, |_, _, event, cx| {
            if matches!(event, InputEvent::Changed) {
                cx.notify();
            }
        })
        .detach();
        // Clicking PS in the topbar again goes back to the covers, the way
        // Biblioteca does.
        let session = AppState::get(cx).session.clone();
        let mut home = session.read(cx).ps_home;
        cx.observe(&session, move |this, session, cx| {
            let now = session.read(cx).ps_home;
            if now != home {
                home = now;
                this.detail.update(cx, DetailView::clear);
            }
        })
        .detach();
        Self {
            ps,
            search,
            detail,
            browser,
            menu: None,
            menu_focus: cx.focus_handle(),
            menu_restore: None,
            list: UniformListScrollHandle::new(),
            row_focus: RefCell::default(),
        }
    }

    /// Escape on a game's page returns to the covers, with the focus on the cover it came from.
    /// Returns whether there was a page to leave.
    pub fn back(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if self.detail.read(cx).selected().is_none() {
            return false;
        }
        self.detail.update(cx, DetailView::clear);
        if let Some(cover) = self.browser.update(cx, |browser, _| browser.take_opened_from()) {
            window.focus(&cover, cx);
        }
        true
    }

    /// The right-click menu of the game in `dir`, from its row or its cover.
    pub fn open_menu(&mut self, dir: PathBuf, position: Point<Pixels>, window: &mut Window, cx: &mut Context<Self>) {
        self.menu_restore = window.focused(cx);
        window.focus(&self.menu_focus, cx);
        self.menu = Some(GameMenu { dir, position });
        cx.notify();
    }

    /// Headers and games in list order: PS4 first, then PS5, each by title,
    /// narrowed by the search box.
    fn rows(&self, cx: &gpui::App) -> Vec<Row> {
        let query = self.search.read(cx).text().trim().to_lowercase();
        let entries = self.ps.read(cx).entries();
        let mut rows = Vec::with_capacity(entries.len() + 2);
        for platform in [Platform::Ps4, Platform::Ps5] {
            let start = rows.len();
            rows.push(Row::Header(platform, 0));
            rows.extend(
                entries
                    .iter()
                    .enumerate()
                    .filter(|(_, entry)| entry.game.platform == platform)
                    .filter(|(_, entry)| query.is_empty() || display_title(&entry.game).to_lowercase().contains(&query))
                    .map(|(index, _)| Row::Game(index)),
            );
            let count = rows.len() - start - 1;
            if count == 0 {
                rows.truncate(start);
            } else {
                rows[start] = Row::Header(platform, count);
            }
        }
        rows
    }

    fn sidebar(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let rows = self.rows(cx);
        let ps = self.ps.read(cx);
        let dirs: HashSet<&PathBuf> = ps.entries().iter().map(|entry| &entry.game.dir).collect();
        self.row_focus.borrow_mut().retain(|dir, _| dirs.contains(dir));
        let body = if rows.is_empty() {
            let message = if ps.scanning() {
                t!("ps.games.scanning")
            } else if ps.entries().is_empty() {
                t!("ps.games.no_dumps")
            } else {
                t!("ps.games.no_match")
            };
            div()
                .p_4()
                .flex()
                .flex_col()
                .items_start()
                .gap_3()
                .child(theme::muted(message))
                .when(!ps.scanning() && ps.entries().is_empty(), |empty| {
                    empty.child(super::adding::add_button(
                        "ps-add-empty",
                        ButtonKind::Secondary,
                        &self.ps,
                        cx,
                    ))
                })
                .into_any_element()
        } else {
            let view = cx.entity();
            uniform_list("ps-games", rows.len(), move |range, _, cx| {
                let cx: &gpui::App = cx;
                let this = view.read(cx);
                let entries = this.ps.read(cx).entries();
                range
                    .filter_map(|index| rows.get(index).map(|row| (index, row)))
                    .filter_map(|(at, row)| match *row {
                        Row::Header(platform, count) => Some(header(platform, count)),
                        Row::Game(index) => entries.get(index).map(|entry| this.row(at, entry, view.clone(), cx)),
                    })
                    .collect()
            })
            .track_scroll(&self.list)
            .flex_1()
            .into_any_element()
        };
        div()
            .w(px(SIDEBAR_WIDTH))
            .flex_none()
            .h_full()
            .flex()
            .flex_col()
            .bg(rgb(theme::BG_PANEL_3))
            .border_r_1()
            .border_color(rgb(theme::LINE))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .p(px(14.))
                    .pb(px(8.))
                    .child(div().flex_1().min_w_0().child(self.search.clone()))
                    .child(super::adding::add_button("ps-add", ButtonKind::Icon, &self.ps, cx))
                    .child(self.refresh_button(cx)),
            )
            .child(body)
            .child(
                div().p(px(14.)).child(
                    button(
                        "ps-compatible-games",
                        ButtonKind::Secondary,
                        t!("ps.games.compatible_list"),
                        false,
                    )
                    .on_click(|_, _, _| {
                        if let Err(err) = crate::desktop::open(COMPATIBLE_GAMES_URL) {
                            tracing::warn!(%err, "could not open the compatible games list");
                        }
                    }),
                ),
            )
    }

    /// Reads the games folder again — for dumps copied in while the
    /// section was open, which nothing watches the folder for.
    fn refresh_button(&self, cx: &Context<Self>) -> impl IntoElement + use<> {
        let scanning = self.ps.read(cx).scanning();
        let ps = self.ps.clone();
        button("ps-rescan", ButtonKind::Icon, "⟳", scanning)
            .tooltip(tooltip(if scanning {
                t!("ps.games.scanning")
            } else {
                t!("ps.games.rescan")
            }))
            .when(!scanning, |b| {
                b.on_click(move |_, _, cx| ps.update(cx, |ps, cx| ps.rescan(cx)))
            })
    }

    fn row(&self, at: usize, entry: &Entry, view: Entity<Self>, cx: &gpui::App) -> AnyElement {
        let game = &entry.game;
        let playable = entry.is_playable(self.ps.read(cx).compat());
        let active = self.detail.read(cx).selected() == Some(&game.dir);
        let running = self.ps.read(cx).is_running(&JobKey::Game(game.dir.clone()), cx);
        let dir = game.dir.clone();
        let focus = self.row_handle(&game.dir, cx);
        nav::focus_ring(div().id(SharedString::from(game.dir.to_string_lossy().into_owned())))
            .track_focus(&focus)
            .on_key_down({
                let (view, dir) = (view.clone(), dir.clone());
                move |event, window, cx| {
                    view.update(cx, |this, cx| this.row_key(at, &dir, event, window, cx));
                }
            })
            .h(px(ROW))
            .w_full()
            .flex()
            .items_center()
            .gap(px(10.))
            .px(px(14.))
            .border_l(px(3.))
            .border_color(gpui::transparent_black())
            .text_size(px(12.5))
            .text_color(rgb(theme::TEXT_PRIMARY))
            .cursor_pointer()
            .hover(|row| row.bg(rgb(theme::BG_PANEL_2)))
            .when(!running && !playable, |row| row.text_color(rgb(theme::TEXT_MUTED)))
            .when(active, |row| {
                row.bg(rgb(theme::BG_PANEL_2))
                    .text_color(rgb(theme::TEXT_BRIGHT))
                    .border_color(rgb(theme::ACCENT_BLUE))
            })
            .on_click({
                let (view, dir) = (view.clone(), dir.clone());
                move |_, _, cx| {
                    view.update(cx, |this, cx| {
                        this.detail.update(cx, |detail, cx| detail.select(dir.clone(), cx));
                    });
                }
            })
            .on_mouse_down(MouseButton::Right, move |event: &MouseDownEvent, window, cx| {
                view.update(cx, |this, cx| this.open_menu(dir.clone(), event.position, window, cx));
            })
            .child(icon(game, ICON))
            .child(sidebar_title(display_title(game), running))
            .when(running, |row| {
                row.child(running_mark(format!("ps-running-{}", game.dir.display())))
            })
            .into_any_element()
    }
}

impl GamesView {
    fn row_handle(&self, dir: &PathBuf, cx: &gpui::App) -> FocusHandle {
        let mut handles = self.row_focus.borrow_mut();
        if let Some(handle) = handles.get(dir) {
            return handle.clone();
        }
        handles
            .entry(dir.clone())
            .or_insert_with(|| cx.focus_handle().tab_stop(true))
            .clone()
    }

    /// The menu key opens a game row's menu; the arrows and Tab focus the next game row, scrolling
    /// it into view first, since the list lays out only the rows in view.
    fn row_key(&mut self, at: usize, dir: &PathBuf, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if nav::is_menu_key(&event.keystroke) {
            let bounds = self.list.0.borrow().base_handle.bounds();
            let offset = self.list.0.borrow().base_handle.offset();
            let position = point(
                bounds.left() + px(80.),
                bounds.top() + offset.y + px(ROW * (at as f32 + 0.5)),
            );
            self.open_menu(dir.clone(), position, window, cx);
            cx.stop_propagation();
            return;
        }
        let Some(step) = Step::of(&event.keystroke) else {
            return;
        };
        let rows = self.rows(cx);
        let target = match step {
            Step::Next => rows
                .get(at + 1..)
                .and_then(|after| after.iter().position(|row| matches!(row, Row::Game(_))))
                .map(|next| at + 1 + next),
            Step::Prev => rows
                .get(..at)
                .and_then(|before| before.iter().rposition(|row| matches!(row, Row::Game(_)))),
        };
        let Some((target, Row::Game(index))) = target.and_then(|target| Some((target, *rows.get(target)?))) else {
            return;
        };
        let Some(next) = self
            .ps
            .read(cx)
            .entries()
            .get(index)
            .map(|entry| entry.game.dir.clone())
        else {
            return;
        };
        self.list.scroll_to_item(target, ScrollStrategy::Nearest);
        window.focus(&self.row_handle(&next, cx), cx);
        cx.stop_propagation();
    }
}

/// Like Biblioteca's shelf titles in its sidebar.
fn header(platform: Platform, count: usize) -> AnyElement {
    div()
        .h(px(ROW))
        .flex()
        .items_end()
        .px(px(14.))
        .pb(px(6.))
        .child(
            theme::display(11.)
                .text_color(rgb(theme::ACCENT_BLUE))
                .child(format!("{} · {count}", platform_label(platform))),
        )
        .into_any_element()
}

impl Render for GamesView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let error = self.ps.read(cx).error().cloned();
        let pane = if self.detail.read(cx).selected().is_some() {
            div()
                .id("ps-detail-pane")
                .size_full()
                .overflow_y_scroll()
                .child(div().pb(px(60.)).child(self.detail.clone()))
                .into_any_element()
        } else {
            self.browser
                .clone()
                .cached(StyleRefinement::default().size_full())
                .into_any_element()
        };
        let menu = self.menu.as_ref().and_then(|target| {
            let view = cx.entity().downgrade();
            let restore = self.menu_restore.clone();
            let dismiss: crate::widgets::menu::Dismiss = Rc::new(move |window, cx| {
                let _ = view.update(cx, |this, cx| {
                    this.menu = None;
                    cx.notify();
                });
                if let Some(restore) = &restore {
                    window.focus(restore, cx);
                }
            });
            context_menu::render(target, &self.ps, &self.detail, &self.menu_focus, window, dismiss, cx)
        });
        if menu.is_none() {
            self.menu = None;
        }
        div().size_full().flex().children(menu).child(self.sidebar(cx)).child(
            div()
                .flex_1()
                .min_w_0()
                .h_full()
                .flex()
                .flex_col()
                .children(error.map(|err| div().p_4().child(theme::error(err))))
                .child(div().flex_1().min_h_0().child(pane)),
        )
    }
}

/// The few words a status has room for.
fn status_text(status: &Status) -> Cow<'static, str> {
    match status {
        Status::Ready => t!("ps.status.ready"),
        Status::NeedsConvert => t!("ps.status.needs_convert"),
        Status::NeedsMerge(_) => t!("ps.status.needs_merge"),
        Status::HasPlayable(_) => t!("ps.status.has_playable"),
        Status::OtherEdition => t!("ps.status.other_edition"),
        Status::Experimental => t!("ps.status.experimental"),
        Status::NeedsVr => t!("ps.status.needs_vr"),
        Status::NeedsUpdate => t!("ps.status.needs_update"),
        Status::Encrypted => t!("ps.status.encrypted"),
        Status::NoEboot => t!("ps.status.no_eboot"),
    }
}

/// What the dump shows as its state: psport's word for the title when it gave one, else decanter's.
fn entry_status_text(entry: &Entry, compat: Option<&Compat>) -> Cow<'static, str> {
    match entry.reported(compat).map(|title| title.state) {
        Some(CompatState::Playable) => t!("ps.status.playable"),
        Some(CompatState::Experimental) => t!("ps.status.experimental"),
        Some(CompatState::Broken) => t!("ps.status.broken"),
        Some(CompatState::Unsupported) => t!("ps.status.unsupported"),
        None => status_text(&entry.status),
    }
}

fn entry_status_color(entry: &Entry, compat: Option<&Compat>) -> u32 {
    if entry
        .reported(compat)
        .is_some_and(|title| title.state == CompatState::Broken)
    {
        theme::ACCENT_GOLD
    } else if entry.is_playable(compat) {
        theme::ACCENT_GREEN
    } else {
        theme::TEXT_MUTED
    }
}

const fn platform_label(platform: Platform) -> &'static str {
    match platform {
        Platform::Ps4 => "PS4",
        Platform::Ps5 => "PS5",
    }
}

/// Without the ™ and ® dumps carry, falling back to the title id for a
/// dump with no name.
pub(super) fn display_title(game: &Game) -> String {
    let title = game.title.replace(['™', '®'], "");
    let title = title.trim();
    if title.is_empty() {
        game.title_id.clone()
    } else {
        title.to_owned()
    }
}

/// `title_ids` by the names the library shows, for a question that names the games it affects.
fn game_names(ps: &Ps, title_ids: &[String]) -> String {
    title_ids
        .iter()
        .map(|title_id| {
            ps.entries()
                .iter()
                .find(|entry| entry.game.title_id == *title_id)
                .map_or_else(|| title_id.clone(), |entry| display_title(&entry.game))
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// The shipped layouts in the UI's language; the user's own keep the name they were given.
fn layout_name<'name>(id: &str, builtin: bool, name: &'name str) -> Cow<'name, str> {
    match (builtin, id) {
        (true, "default") => t!("ps.layouts.default"),
        (true, "code-vein") => t!("ps.layouts.code_vein"),
        (true, "") => t!("ps.layouts.custom"),
        _ => Cow::Borrowed(name),
    }
}

/// What a game's tab says about whose settings it shows.
fn scope_note(scope: IniScope) -> Cow<'static, str> {
    t!(match scope {
        IniScope::Title => "ps.scope.title",
        IniScope::OlderPort => "ps.scope.older_port",
        IniScope::UnusualTitleId => "ps.scope.unusual_title_id",
    })
}

/// A file of the dump's `sce_sys`, as the art cache takes it.
fn art_url(game: &Game, file: &str) -> String {
    crate::art::file_url(&game.dir.join("sce_sys").join(file))
}

/// `sce_sys/icon0.png` is 512 px; it goes through the art cache to be
/// stored and decoded at the size drawn instead.
fn icon(game: &Game, size: f32) -> impl IntoElement + use<> {
    img(crate::art::source(&art_url(game, "icon0.png"), gpui::size(size, size)))
        .size(px(size))
        .flex_none()
        .rounded(px(theme::RADIUS_SM))
        .object_fit(ObjectFit::Cover)
        .bg(rgb(theme::BG_PANEL_LIGHT))
        .with_fallback(move || {
            div()
                .size(px(size))
                .rounded(px(theme::RADIUS_SM))
                .bg(rgb(theme::BG_PANEL_LIGHT))
                .into_any_element()
        })
}
