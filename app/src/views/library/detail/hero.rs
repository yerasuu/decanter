use decanter_core::{Game, Playtime, Storefront};
use gpui::{Context, Div, FontWeight, ObjectFit, SharedString, Stateful, div, img, prelude::*, px, relative, rgb};
use rust_i18n::t;

use super::DetailView;
use crate::theme;
use crate::views::library::{actions, data, fmt};
use crate::widgets::{ButtonKind, button, tooltip};

pub(super) fn hero(
    game: &Game,
    unlocked: usize,
    total: usize,
    playtime: Playtime,
    is_fav: bool,
    cx: &Context<DetailView>,
) -> Div {
    let storefront = game.storefront;
    hero_backdrop(game)
        .child(
            div()
                .absolute()
                .top(px(20.))
                .right(px(24.))
                .flex()
                .gap(px(10.))
                .when(storefront == Storefront::Gog, |stats| {
                    stats.child(hero_stat(
                        format!("{unlocked}/{total}"),
                        t!("library.detail.hero.achievements"),
                    ))
                })
                .child(hero_stat(
                    fmt::duration(playtime.seconds_total),
                    t!("library.detail.hero.played"),
                )),
        )
        .child(
            div()
                .relative()
                .w_full()
                .p(px(32.))
                .flex()
                .flex_col()
                .gap(px(6.))
                // In a row of its own: the column stretches its children, which
                // drew the badge as a bar across the whole hero.
                .child(div().flex().child(if storefront == Storefront::Gog {
                    theme::badge("GOG", true)
                } else {
                    theme::badge(t!("library.detail.external"), false)
                }))
                // Beside the title rather than at the end of the action bar: it
                // says something about the game, not something to do with it,
                // and buried after DESINSTALAR is the last place anyone looks
                // for it.
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(12.))
                        .w_full()
                        .child(
                            theme::display(34.)
                                .flex_1()
                                .min_w_0()
                                .text_color(rgb(theme::TEXT_BRIGHT))
                                .child(game.title.to_uppercase()),
                        )
                        .child(favorite_button(game, is_fav, cx)),
                ),
        )
}

fn hero_backdrop(game: &Game) -> Div {
    let art = game.background_url.as_deref().or(game.cover_url.as_deref());
    let hero = div()
        .relative()
        .min_h(px(300.))
        .overflow_hidden()
        .flex()
        .items_end()
        .bg(rgb(theme::BG_PANEL_2));
    // The inline `background-image` replaced the whole gradient stack in the
    // webview build, so the gradients only ever showed with no art at all.
    match art {
        Some(url) => hero.child(
            // Wide enough for the detail pane on a maximized 1080p window;
            // the hero's height follows its text, 300 px at the least.
            img(crate::art::source(url, gpui::size(1600., 400.)))
                .absolute()
                .top_0()
                .left_0()
                .size_full()
                .object_fit(ObjectFit::Cover),
        ),
        None => hero
            .child(
                // GPUI has no radial gradient: a diagonal two-stop linear
                // one stands in for the light-at-top-left radial.
                div().absolute().top_0().left_0().size_full().bg(gpui::linear_gradient(
                    150.,
                    gpui::linear_color_stop(rgb(theme::BG_PANEL_LIGHT), 0.),
                    gpui::linear_color_stop(rgb(theme::BG_PANEL_3), 1.),
                )),
            )
            .child(
                // 0 → .55 at 55% → 1 is a straight line, so two stops are exact.
                div().absolute().top_0().left_0().size_full().bg(gpui::linear_gradient(
                    180.,
                    gpui::linear_color_stop(theme::alpha(theme::BG_DEEP, 0.), 0.),
                    gpui::linear_color_stop(theme::alpha(theme::BG_DEEP, 1.), 1.),
                )),
            ),
    }
}

fn favorite_button(game: &Game, is_fav: bool, cx: &Context<DetailView>) -> Stateful<Div> {
    let account_id = game.account_id.clone();
    let storefront = game.storefront;
    let product_id = game.id.clone();
    button("favorite", ButtonKind::Icon, if is_fav { "★" } else { "☆" }, false)
        .text_size(px(26.))
        .line_height(relative(1.))
        .px(px(6.))
        .py(px(4.))
        .when(is_fav, |star| star.text_color(rgb(theme::ACCENT_GOLD)))
        .tooltip(tooltip(if is_fav {
            t!("library.detail.favorite.remove")
        } else {
            t!("library.detail.favorite.add")
        }))
        .on_click(cx.listener(move |_, _, _, cx| {
            data::set_favorite(&account_id, storefront, &product_id, !is_fav);
            // The sidebar and cover browser both have a FAVORITOS shelf;
            // without this they'd keep showing the pre-toggle grouping. It
            // also re-reads the star here, since this pane caches it.
            actions::bump_organisation(cx);
        }))
}

/// `.hero-stat`. The webview blurred what was behind it
/// (`backdrop-filter`), which GPUI can't; a more opaque fill keeps the
/// number readable over busy art instead.
fn hero_stat(value: String, label: impl Into<SharedString>) -> Div {
    div()
        .flex()
        .flex_col()
        .items_center()
        .bg(theme::alpha(theme::BG_DEEP, 0.85))
        .px_4()
        .py_2()
        .rounded(px(theme::RADIUS))
        .border_1()
        .border_color(rgb(theme::LINE))
        .child(
            theme::mono()
                .text_size(px(19.))
                .font_weight(FontWeight::BOLD)
                .text_color(rgb(theme::TEXT_BRIGHT))
                .child(value),
        )
        .child(
            div()
                .mt(px(2.))
                .text_size(px(10.))
                .text_color(rgb(theme::TEXT_MUTED))
                .child(label.into()),
        )
}
