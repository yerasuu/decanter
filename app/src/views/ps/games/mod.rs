//! Juegos: the profile's dumps in a sidebar, and beside it either the
//! cover browser or the selected game — the same shape as Biblioteca.

mod browser;
mod context_menu;
mod detail;
mod extras;
mod gamepad;
mod graphics;
mod keyboard;
mod mods;
mod online;
mod runs;

use decanter_ps::bbport::IniScope;
use decanter_ps::library::{Game, Platform};
use decanter_ps::status::Status;
use std::borrow::Cow;
use std::path::PathBuf;
use std::rc::Rc;

use gpui::{
    AnyElement, Context, Entity, MouseButton, MouseDownEvent, ObjectFit, Pixels, Point, SharedString, StyleRefinement,
    Window, div, img, prelude::*, px, rgb, uniform_list,
};
use rust_i18n::t;

use crate::i18n::Text;
use crate::state::{AppState, JobKey, Ps};
use crate::theme;
use crate::widgets::{ButtonKind, InputEvent, TextInput, button, running_mark, sidebar_title, tooltip};
use browser::BrowserView;
use context_menu::GameMenu;
use detail::DetailView;

const SIDEBAR_WIDTH: f32 = 292.;
const ICON: f32 = 26.;
/// One height for headers and games alike, so the list can be uniform.
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
        }
    }

    /// The right-click menu of the game in `dir`, from its row or its cover.
    pub fn open_menu(&mut self, dir: PathBuf, position: Point<Pixels>, cx: &mut Context<Self>) {
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
                    .filter_map(|index| rows.get(index))
                    .filter_map(|row| match *row {
                        Row::Header(platform, count) => Some(header(platform, count)),
                        Row::Game(index) => entries
                            .get(index)
                            .map(|entry| this.row(&entry.game, &entry.status, view.clone(), cx)),
                    })
                    .collect()
            })
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

    fn row(&self, game: &Game, status: &Status, view: Entity<Self>, cx: &gpui::App) -> AnyElement {
        let active = self.detail.read(cx).selected() == Some(&game.dir);
        let running = self.ps.read(cx).is_running(&JobKey::Game(game.dir.clone()), cx);
        let dir = game.dir.clone();
        div()
            .id(SharedString::from(game.dir.to_string_lossy().into_owned()))
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
            .when(!running && !status.playable(), |row| {
                row.text_color(rgb(theme::TEXT_MUTED))
            })
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
            .on_mouse_down(MouseButton::Right, move |event: &MouseDownEvent, _, cx| {
                view.update(cx, |this, cx| this.open_menu(dir.clone(), event.position, cx));
            })
            .child(icon(game, ICON))
            .child(sidebar_title(display_title(game), running))
            .when(running, |row| {
                row.child(running_mark(format!("ps-running-{}", game.dir.display())))
            })
            .into_any_element()
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
            let dismiss: crate::widgets::menu::Dismiss = Rc::new(move |_, cx| {
                let _ = view.update(cx, |this, cx| {
                    this.menu = None;
                    cx.notify();
                });
            });
            context_menu::render(target, &self.ps, &self.detail, window, dismiss, cx)
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

/// The shipped layouts in the UI's language; the user's own keep the name they were given.
fn layout_name<'name>(id: &str, builtin: bool, name: &'name str) -> Cow<'name, str> {
    match (builtin, id) {
        (true, "default") => t!("ps.layouts.default"),
        (true, "code-vein") => t!("ps.layouts.code_vein"),
        (true, "") => t!("ps.layouts.custom"),
        _ => Cow::Borrowed(name),
    }
}

/// What a game's tab says about whose settings it shows: `own` is the tab's wording for the game's
/// own port ini, since some tabs still show settings every game shares.
fn scope_note(scope: IniScope, own: &'static str) -> Cow<'static, str> {
    t!(match scope {
        IniScope::Title => own,
        IniScope::OlderPort => "ps.scope.older_port",
        IniScope::UnusualTitleId => "ps.scope.unusual_title_id",
    })
}

/// A file of the dump's `sce_sys`, as the art cache takes it.
fn art_url(game: &Game, file: &str) -> String {
    format!("file://{}", game.dir.join("sce_sys").join(file).display())
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
