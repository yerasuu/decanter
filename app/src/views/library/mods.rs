//! GOG's mods list: every mod the catalog offers, which of them this
//! profile already owns, and the gog.com page to add the rest from, since
//! GOG documents no way to add one from outside its store.

use std::collections::HashSet;

use decanter_gog::GogMod;
use gpui::{App, Div, Entity, SharedString, Stateful, div, prelude::*, px, rgb};
use rust_i18n::t;

use super::LibraryModel;
use crate::state::{AppState, ModsState};
use crate::theme;
use crate::widgets::{ButtonKind, button, tooltip};

pub(super) fn pane(model: &Entity<LibraryModel>, cx: &App) -> Stateful<Div> {
    let library = AppState::get(cx).library.read(cx);
    let owned: HashSet<&str> = library
        .games
        .iter()
        .filter(|entry| entry.is_mod)
        .map(|entry| entry.game.id.as_str())
        .collect();
    let body = match &library.mods {
        ModsState::Loading => theme::empty(t!("library.mods.loading")),
        ModsState::Unavailable => theme::empty(t!("library.mods.unavailable")),
        ModsState::Ready(mods) if mods.is_empty() => theme::empty(t!("library.mods.none")),
        ModsState::Ready(mods) => div().flex().flex_col().children(
            mods.iter()
                .map(|product| mod_row(product, owned.contains(product.id.as_str()))),
        ),
    };
    let close_model = model.clone();
    div().id("gog-mods").size_full().overflow_y_scroll().child(
        div()
            .p(px(32.))
            .flex()
            .flex_col()
            .gap(px(14.))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .child(
                        theme::display(22.)
                            .flex_1()
                            .text_color(rgb(theme::TEXT_BRIGHT))
                            .child(t!("library.mods.heading")),
                    )
                    .child(
                        button("gog-mods-close", ButtonKind::Icon, "✕", false)
                            .tooltip(tooltip(t!("library.mods.close")))
                            .on_click(move |_, _, cx| close_model.update(cx, |model, cx| model.show_mods(false, cx))),
                    ),
            )
            .child(theme::muted(t!("library.mods.note")).text_size(px(12.5)))
            .child(body),
    )
}

fn mod_row(product: &GogMod, owned: bool) -> Div {
    let url = product.store_url.clone();
    div()
        .flex()
        .items_center()
        .gap_3()
        .py(px(10.))
        .border_b_1()
        .border_color(rgb(theme::LINE))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .text_color(rgb(theme::TEXT_PRIMARY))
                .child(product.title.clone()),
        )
        .when(owned, |row| row.child(theme::badge(t!("library.mods.owned"), false)))
        .child(
            button(
                SharedString::from(format!("gog-mod-{}", product.id)),
                ButtonKind::Secondary,
                t!("library.mods.view"),
                false,
            )
            .on_click(move |_, _, _| {
                if let Err(err) = crate::desktop::open(&url) {
                    tracing::warn!(url, %err, "could not open the GOG mod page");
                }
            }),
        )
}
