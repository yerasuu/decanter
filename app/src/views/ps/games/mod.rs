//! Juegos: the profile's dumps in a sidebar, and beside it either the
//! cover browser or the selected game — the same shape as Biblioteca.

mod browser;
mod detail;
mod extras;
mod gamepad;
mod graphics;
mod keyboard;
mod mods;

use decanter_ps::library::{Game, Platform};
use decanter_ps::status::Status;
use gpui::{
    AnyElement, Context, Entity, ObjectFit, SharedString, StyleRefinement, Window, div, img, prelude::*, px, rgb,
    uniform_list,
};

use crate::state::{AppState, Ps};
use crate::theme;
use crate::widgets::{InputEvent, TextInput, tooltip};
use browser::BrowserView;
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
}

impl GamesView {
    pub fn new(ps: Entity<Ps>, cx: &mut Context<Self>) -> Self {
        cx.observe(&ps, |_, _, cx| cx.notify()).detach();
        let detail = cx.new(|cx| DetailView::new(ps.clone(), cx));
        cx.observe(&detail, |_, _, cx| cx.notify()).detach();
        let browser = cx.new(|cx| BrowserView::new(ps.clone(), detail.clone(), cx));
        let search = cx.new(|cx| TextInput::new("Buscar", cx));
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
        }
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
                "Buscando juegos…"
            } else if ps.dirs().is_empty() {
                "Añadí una carpeta con tus volcados en Ajustes."
            } else if ps.entries().is_empty() {
                "No hay volcados de PS4 o PS5 en tus carpetas."
            } else {
                "Nada coincide con la búsqueda."
            };
            div().p_4().child(theme::muted(message)).into_any_element()
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
            .child(div().p(px(14.)).pb(px(8.)).child(self.search.clone()))
            .child(body)
    }

    fn row(&self, game: &Game, status: &Status, view: Entity<Self>, cx: &gpui::App) -> AnyElement {
        let active = self.detail.read(cx).selected() == Some(&game.dir);
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
            .when(active, |row| {
                row.bg(rgb(theme::BG_PANEL_2))
                    .text_color(rgb(theme::TEXT_BRIGHT))
                    .border_color(rgb(theme::ACCENT_BLUE))
            })
            .on_click(move |_, _, cx| {
                view.update(cx, |this, cx| {
                    this.detail.update(cx, |detail, cx| detail.select(dir.clone(), cx));
                });
            })
            .child(icon(game, ICON))
            .child(div().flex_1().min_w_0().truncate().child(display_title(game)))
            .when(status.playable(), |row| {
                row.child(
                    div()
                        .id(SharedString::from(format!("ps-ready-{}", game.dir.display())))
                        .size(px(6.))
                        .flex_none()
                        .rounded_full()
                        .bg(rgb(theme::ACCENT_GREEN))
                        .tooltip(tooltip("Listo para jugar")),
                )
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
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let error = self.ps.read(cx).error().map(str::to_owned);
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
        div().size_full().flex().child(self.sidebar(cx)).child(
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
const fn status_text(status: &Status) -> &'static str {
    match status {
        Status::Ready => "Listo",
        Status::NeedsConvert => "Se convierte al jugar",
        Status::NeedsMerge(_) => "Falta fusionar 1.09",
        Status::HasPlayable(_) => "Usa la copia 1.09",
        Status::OtherEdition => "Otra edición",
        Status::NeedsUpdate => "Necesita 1.09",
        Status::Encrypted => "eboot cifrado",
        Status::NoEboot => "Sin eboot.bin",
        Status::Unsupported => "No soportado",
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
fn display_title(game: &Game) -> String {
    let title = game.title.replace(['™', '®'], "");
    let title = title.trim();
    if title.is_empty() {
        game.title_id.clone()
    } else {
        title.to_owned()
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_title_drops_marks_and_falls_back_to_the_id() {
        let game = |title: &str| Game {
            dir: std::path::PathBuf::from("/g"),
            platform: Platform::Ps4,
            title_id: "CUSA00900".into(),
            title: title.into(),
            version: "01.09".into(),
            category: "gd".into(),
            has_modules: true,
        };
        assert_eq!(display_title(&game("Bloodborne™")), "Bloodborne");
        assert_eq!(display_title(&game(" ® ")), "CUSA00900");
    }
}
