//! The covers of every dump, a shelf per platform, shown while no game is
//! selected — Biblioteca's shelf browser for PS games.

use decanter_ps::library::Platform;
use decanter_ps::status::Entry;
use gpui::{
    BoxShadow, Context, Entity, MouseButton, MouseDownEvent, ObjectFit, SharedString, WeakEntity, Window, div, img,
    linear_color_stop, linear_gradient, point, prelude::*, px, rgb,
};

use super::GamesView;
use super::detail::DetailView;
use super::{art_url, display_title, platform_label, status_text};
use crate::state::Ps;
use crate::theme;

/// Square, like `icon0.png`.
const COVER: f32 = 156.;
const GROUP: &str = "ps-cover";

pub struct BrowserView {
    ps: Entity<Ps>,
    detail: Entity<DetailView>,
    /// Holds the game menu a right-click on a cover opens.
    games: WeakEntity<GamesView>,
}

impl BrowserView {
    pub fn new(
        ps: Entity<Ps>,
        detail: Entity<DetailView>,
        games: WeakEntity<GamesView>,
        cx: &mut Context<Self>,
    ) -> Self {
        cx.observe(&ps, |_, _, cx| cx.notify()).detach();
        Self { ps, detail, games }
    }

    fn card(&self, entry: &Entry) -> impl IntoElement + use<> {
        let game = &entry.game;
        let dir = game.dir.clone();
        let detail = self.detail.clone();
        let (games, menu_dir) = (self.games.clone(), game.dir.clone());
        let ready = entry.status.playable();
        let art = div()
            .id("ps-cover-art")
            .relative()
            .size(px(COVER))
            .rounded(px(theme::RADIUS))
            .overflow_hidden()
            .bg(rgb(theme::BG_PANEL_2))
            .group_hover(GROUP, |art| {
                art.top(px(-4.)).shadow(vec![
                    BoxShadow {
                        color: rgb(theme::ACCENT_BLUE).into(),
                        offset: point(px(0.), px(0.)),
                        blur_radius: px(0.),
                        spread_radius: px(2.),
                        inset: false,
                    },
                    BoxShadow {
                        color: gpui::black().opacity(0.45),
                        offset: point(px(0.), px(10.)),
                        blur_radius: px(22.),
                        spread_radius: px(0.),
                        inset: false,
                    },
                ])
            })
            .group_active(GROUP, |art| art.top(px(-1.)))
            .child(
                img(crate::art::source(
                    &art_url(game, "icon0.png"),
                    gpui::size(COVER, COVER),
                ))
                .absolute()
                .top_0()
                .left_0()
                .size_full()
                .object_fit(ObjectFit::Cover)
                .with_fallback(|| div().into_any_element()),
            )
            // Only the bottom edge darkens, for the status pill; the art
            // itself already carries the game's name.
            .child(div().absolute().top_0().left_0().size_full().bg(linear_gradient(
                180.,
                linear_color_stop(theme::alpha(theme::BG_DEEP, 0.), 0.6),
                linear_color_stop(theme::alpha(theme::BG_DEEP, 0.8), 1.),
            )))
            .child(
                div()
                    .absolute()
                    .bottom(px(8.))
                    .left(px(8.))
                    .flex()
                    .items_center()
                    .gap(px(5.))
                    .px(px(6.))
                    .py(px(1.))
                    .rounded(px(theme::RADIUS_SM))
                    .bg(theme::alpha(theme::BG_DEEP, 0.85))
                    .text_size(px(10.))
                    .text_color(rgb(if ready { theme::ACCENT_GREEN } else { theme::TEXT_MUTED }))
                    .child(status_text(&entry.status)),
            );
        div()
            .id(SharedString::from(format!("ps-cover-{}", game.dir.display())))
            .group(GROUP)
            .w(px(COVER))
            .flex()
            .flex_col()
            .gap(px(8.))
            .cursor_pointer()
            .on_click(move |_, _, cx| detail.update(cx, |detail, cx| detail.select(dir.clone(), cx)))
            .on_mouse_down(MouseButton::Right, move |event: &MouseDownEvent, _, cx| {
                let _ = games.update(cx, |games, cx| games.open_menu(menu_dir.clone(), event.position, cx));
            })
            .child(art)
            .child(
                div()
                    .text_size(px(12.5))
                    .text_color(rgb(theme::TEXT_PRIMARY))
                    .line_clamp(2)
                    .child(display_title(game)),
            )
    }
}

impl Render for BrowserView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let ps = self.ps.read(cx);
        let shelves: Vec<_> = [Platform::Ps4, Platform::Ps5]
            .into_iter()
            .filter_map(|platform| {
                let cards: Vec<_> = ps
                    .entries()
                    .iter()
                    .filter(|entry| entry.game.platform == platform)
                    .map(|entry| self.card(entry).into_any_element())
                    .collect();
                (!cards.is_empty()).then(|| {
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(14.))
                        .child(theme::display(15.).text_color(rgb(theme::TEXT_MUTED)).child(format!(
                            "{} · {}",
                            platform_label(platform),
                            cards.len()
                        )))
                        .child(div().flex().flex_wrap().gap(px(18.)).children(cards))
                })
            })
            .collect();
        let empty = shelves.is_empty();
        div().id("ps-browser").size_full().overflow_y_scroll().child(
            div()
                .p(px(32.))
                .flex()
                .flex_col()
                .gap(px(32.))
                .when(empty, |page| {
                    page.child(theme::empty(
                        "Todavía no hay juegos: copiá tus volcados a la carpeta de juegos (ver Ajustes).",
                    ))
                })
                .children(shelves),
        )
    }
}
