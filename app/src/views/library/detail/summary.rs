use decanter_core::{Game, Playtime};
use gpui::{Div, div, prelude::*, px, relative, rgb};
use rust_i18n::t;

use crate::theme;
use crate::views::library::fmt;
use crate::widgets::stat_card;

pub(super) fn summary_tab(game: &Game, playtime: Playtime) -> Div {
    let stats = div()
        .flex()
        .flex_wrap()
        .gap(px(14.))
        .mt(px(20.))
        .mx(px(32.))
        .child(stat_card(
            fmt::duration(playtime.seconds_total),
            &t!("library.detail.summary.playtime"),
        ))
        .children(
            playtime
                .last_played_at
                .and_then(fmt::unix_date)
                .map(|last| stat_card(last, &t!("library.detail.summary.last_session"))),
        )
        .children(
            game.rating
                .clone()
                .map(|rating| stat_card(rating, &t!("library.detail.summary.rating"))),
        )
        .children(
            game.release_date
                .and_then(fmt::unix_date)
                .map(|date| stat_card(date, &t!("library.detail.summary.release"))),
        );

    // HTML collapsed the two spaces the webview put after the developer
    // into one, so that's what is shown between the two credits.
    let credits = match (&game.developer, &game.publisher) {
        (None, None) => None,
        (Some(dev), None) => Some(t!("library.detail.summary.developer", developer = dev)),
        (None, Some(publisher)) => Some(t!("library.detail.summary.publisher", publisher = publisher)),
        (Some(dev), Some(publisher)) => Some(t!(
            "library.detail.summary.developer_publisher",
            developer = dev,
            publisher = publisher
        )),
    };
    let genres =
        (!game.genres.is_empty()).then(|| t!("library.detail.summary.genres", genres = game.genres.join(", ")));
    let paragraph = |text: String| {
        div()
            .mb(px(10.))
            .line_height(relative(1.6))
            .text_color(rgb(theme::TEXT_MUTED))
            .child(text)
    };
    let text = div()
        .mt(px(20.))
        .mx(px(32.))
        .mb(px(40.))
        .max_w(px(720.))
        .children(game.description.clone().map(paragraph))
        .children(credits.map(|text| paragraph(text.into_owned())))
        .children(genres.map(|text| paragraph(text.into_owned())));

    div().flex().flex_col().child(stats).child(text)
}
