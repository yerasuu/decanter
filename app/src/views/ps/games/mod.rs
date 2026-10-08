//! Juegos: the profile's dumps in a list on the left, the selected one on
//! the right.

mod controls;
mod detail;
mod extras;
mod graphics;
mod mods;

use decanter_ps::library::{Game, Platform};
use decanter_ps::status::Status;
use gpui::{AnyElement, Context, Entity, ObjectFit, SharedString, Window, div, img, prelude::*, px, rgb, uniform_list};

use crate::state::Ps;
use crate::theme;
use detail::DetailView;

const ICON: f32 = 36.;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Filter {
    All,
    Ps4,
    Ps5,
}

const FILTERS: [(Filter, &str); 3] = [(Filter::All, "Todos"), (Filter::Ps4, "PS4"), (Filter::Ps5, "PS5")];

impl Filter {
    fn keeps(self, game: &Game) -> bool {
        match self {
            Self::All => true,
            Self::Ps4 => game.platform == Platform::Ps4,
            Self::Ps5 => game.platform == Platform::Ps5,
        }
    }
}

pub struct GamesView {
    ps: Entity<Ps>,
    filter: Filter,
    detail: Entity<DetailView>,
}

impl GamesView {
    pub fn new(ps: Entity<Ps>, cx: &mut Context<Self>) -> Self {
        cx.observe(&ps, |_, _, cx| cx.notify()).detach();
        let detail = cx.new(|cx| DetailView::new(ps.clone(), cx));
        cx.observe(&detail, |_, _, cx| cx.notify()).detach();
        Self {
            ps,
            filter: Filter::All,
            detail,
        }
    }

    fn sidebar(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let entity = cx.entity();
        let filters = crate::widgets::tabs("ps-filter", &FILTERS, self.filter, move |filter, _, cx| {
            entity.update(cx, |this, cx| {
                this.filter = filter;
                cx.notify();
            });
        })
        .px(px(14.))
        .pt_3();
        let ps = self.ps.read(cx);
        let visible: Vec<usize> = ps
            .entries()
            .iter()
            .enumerate()
            .filter(|(_, entry)| self.filter.keeps(&entry.game))
            .map(|(index, _)| index)
            .collect();
        let body = if visible.is_empty() {
            let message = if ps.scanning() {
                "Buscando juegos…"
            } else if ps.dirs().is_empty() {
                "Añadí una carpeta con tus volcados en Ajustes."
            } else {
                "No hay volcados de PS4 o PS5 en tus carpetas."
            };
            div().p_4().child(theme::muted(message)).into_any_element()
        } else {
            let view = cx.entity();
            uniform_list("ps-games", visible.len(), move |range, _, cx| {
                let cx: &gpui::App = cx;
                let this = view.read(cx);
                let ps = this.ps.read(cx);
                range
                    .filter_map(|row| visible.get(row).and_then(|&index| ps.entries().get(index)))
                    .map(|entry| this.row(&entry.game, &entry.status, view.clone(), cx))
                    .collect()
            })
            .flex_1()
            .into_any_element()
        };
        div()
            .w(px(300.))
            .flex_none()
            .h_full()
            .flex()
            .flex_col()
            .bg(rgb(theme::BG_PANEL_3))
            .border_r_1()
            .border_color(rgb(theme::LINE))
            .child(filters)
            .child(body)
    }

    fn row(&self, game: &Game, status: &Status, view: Entity<Self>, cx: &gpui::App) -> AnyElement {
        let active = self.detail.read(cx).selected() == Some(&game.dir);
        let dir = game.dir.clone();
        let subtitle = format!("{} · {} · {}", platform_label(game), game.version, status_text(status));
        div()
            .id(SharedString::from(game.dir.to_string_lossy().into_owned()))
            .h(px(52.))
            .flex()
            .items_center()
            .gap_3()
            .px(px(14.))
            .cursor_pointer()
            .when(active, |row| row.bg(rgb(theme::BG_PANEL)))
            .when(!active, |row| row.hover(|row| row.bg(rgb(theme::BG_PANEL_2))))
            .on_click(move |_, _, cx| {
                view.update(cx, |this, cx| {
                    this.detail.update(cx, |detail, cx| detail.select(dir.clone(), cx));
                });
            })
            .child(icon(game))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .min_w_0()
                    .child(
                        div()
                            .truncate()
                            .text_color(rgb(if active {
                                theme::TEXT_BRIGHT
                            } else {
                                theme::TEXT_PRIMARY
                            }))
                            .child(display_title(game)),
                    )
                    .child(
                        div()
                            .truncate()
                            .text_size(px(11.))
                            .text_color(rgb(theme::TEXT_MUTED))
                            .child(subtitle),
                    ),
            )
            .into_any_element()
    }
}

impl Render for GamesView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let error = self.ps.read(cx).error().map(str::to_owned);
        div().size_full().flex().child(self.sidebar(cx)).child(
            div()
                .flex_1()
                .min_w_0()
                .h_full()
                .flex()
                .flex_col()
                .children(error.map(|err| div().p_4().child(theme::error(err))))
                .child(div().flex_1().min_h_0().child(self.detail.clone())),
        )
    }
}

/// The few words a list row has room for.
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

fn platform_label(game: &Game) -> &'static str {
    match game.platform {
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

/// `sce_sys/icon0.png` is 512 px; it goes through the art cache to be
/// stored and decoded at the size drawn here instead.
fn icon(game: &Game) -> impl IntoElement + use<> {
    let url = format!("file://{}", game.dir.join("sce_sys/icon0.png").display());
    img(crate::art::source(&url, gpui::size(ICON, ICON)))
        .size(px(ICON))
        .flex_none()
        .rounded(px(theme::RADIUS_SM))
        .object_fit(ObjectFit::Cover)
        .bg(rgb(theme::BG_PANEL_LIGHT))
        .with_fallback(|| {
            div()
                .size(px(ICON))
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
