use decanter_core::{Game, Playtime};
use gpui::{Div, div, prelude::*, px, relative, rgb};
use rust_i18n::t;

use crate::theme;
use crate::views::library::fmt;

pub(super) fn summary_tab(game: &Game, playtime: Playtime) -> Div {
    let facts: Vec<(String, String)> = [
        game.developer
            .clone()
            .map(|developer| (t!("library.detail.summary.developer_label").into_owned(), developer)),
        game.publisher
            .clone()
            .map(|publisher| (t!("library.detail.summary.publisher_label").into_owned(), publisher)),
        (!game.genres.is_empty()).then(|| {
            (
                t!("library.detail.summary.genres_label").into_owned(),
                game.genres.join(", "),
            )
        }),
        game.release_date
            .and_then(fmt::utc_date)
            .map(|date| (t!("library.detail.summary.release").into_owned(), date)),
        game.rating
            .as_deref()
            .map(|rating| (t!("library.detail.summary.rating").into_owned(), fmt::rating(rating))),
        playtime
            .last_played_at
            .and_then(fmt::unix_date)
            .map(|last| (t!("library.detail.summary.last_session").into_owned(), last)),
    ]
    .into_iter()
    .flatten()
    .collect();
    let count = facts.len();
    let card = (count > 0).then(|| {
        theme::card()
            .mt(px(20.))
            .mx(px(32.))
            .max_w(px(720.))
            .children(facts.into_iter().enumerate().map(|(index, (label, value))| {
                theme::compact_row(index + 1 == count)
                    .py(px(12.))
                    .child(
                        div()
                            .w(px(140.))
                            .flex_none()
                            .text_size(px(12.))
                            .text_color(rgb(theme::TEXT_MUTED))
                            .child(label),
                    )
                    .child(div().flex_1().min_w_0().child(value))
            }))
    });
    let description = game.description.clone().map(|text| {
        div()
            .mt(px(20.))
            .mx(px(32.))
            .max_w(px(720.))
            .line_height(relative(1.6))
            .text_color(rgb(theme::TEXT_MUTED))
            .child(text)
    });
    div().flex().flex_col().pb(px(40.)).children(description).children(card)
}
