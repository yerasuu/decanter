//! «Añadir volcado…» and what follows it: the copy-or-move question, the
//! copy's progress with its cancel, and how it ended. Shown above every tab
//! of the section, since a drop on the window can start it from anywhere.

use std::sync::Arc;

use gpui::{AnyElement, App, Div, Entity, FontWeight, SharedString, div, prelude::*, px, rgb};
use rust_i18n::t;

use super::games::display_title;
use crate::external::transfer::{Mode, Progress};
use crate::i18n::Text;
use crate::state::{Ps, PsAdding, PsOffer};
use crate::theme;
use crate::widgets::{ButtonKind, button, progress_bar, tooltip};

/// The button that starts an add, off while one is under way.
pub fn add_button(id: &'static str, kind: ButtonKind, ps: &Entity<Ps>, cx: &App) -> impl IntoElement + use<> {
    let busy = ps.read(cx).adding().is_some();
    let label = if matches!(kind, ButtonKind::Icon) {
        SharedString::from("+")
    } else {
        t!("ps.add.button").into()
    };
    let ps = ps.clone();
    button(id, kind, label, busy)
        .tooltip(tooltip(t!("ps.add.button")))
        .when(!busy, |b| b.on_click(move |_, _, cx| ps.update(cx, Ps::pick_dump)))
}

pub fn panel(ps: &Entity<Ps>, cx: &App) -> Option<AnyElement> {
    let state = ps.read(cx);
    let body = match (state.adding(), state.add_notice()) {
        (Some(PsAdding::Asking(offer)), _) => asking(offer, ps),
        (Some(PsAdding::Running { offer, mode, progress }), _) => running(offer, *mode, progress, ps),
        (None, Some((text, is_error))) => notice(text, *is_error, ps),
        (None, None) => return None,
    };
    Some(
        theme::card()
            .mx(px(18.))
            .mt_3()
            .p_4()
            .text_size(px(12.5))
            .text_color(rgb(theme::TEXT_PRIMARY))
            .child(body)
            .into_any_element(),
    )
}

fn heading(text: impl Into<SharedString>) -> Div {
    div()
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(rgb(theme::TEXT_BRIGHT))
        .child(text.into())
}

fn asking(offer: &PsOffer, ps: &Entity<Ps>) -> Div {
    let details = t!(
        "ps.add.details",
        source = offer.source.display(),
        target = offer.target.display(),
        size = crate::format::bytes(offer.size)
    );
    let (copy, move_, cancel) = (ps.clone(), ps.clone(), ps.clone());
    div()
        .flex()
        .flex_col()
        .gap(px(6.))
        .child(heading(t!(
            "ps.add.title",
            title = display_title(&offer.game),
            title_id = offer.game.title_id
        )))
        .child(theme::muted(details).text_size(px(12.)))
        .children(
            offer
                .free
                .map(|free| theme::muted(t!("ps.add.free", free = crate::format::bytes(free))).text_size(px(12.))),
        )
        .child(t!("ps.add.question"))
        .child(
            div()
                .flex()
                .gap(px(8.))
                .mt_1()
                .child(
                    button("ps-add-copy", ButtonKind::Primary, t!("ps.add.copy"), false)
                        .on_click(move |_, _, cx| copy.update(cx, |ps, cx| ps.start_add(Mode::Copy, cx))),
                )
                .child(
                    button("ps-add-move", ButtonKind::Secondary, t!("ps.add.move"), false)
                        .on_click(move |_, _, cx| move_.update(cx, |ps, cx| ps.start_add(Mode::Move, cx))),
                )
                .child(
                    button("ps-add-cancel", ButtonKind::Secondary, t!("ps.add.cancel"), false)
                        .on_click(move |_, _, cx| cancel.update(cx, Ps::cancel_add)),
                ),
        )
}

fn running(offer: &PsOffer, mode: Mode, progress: &Arc<Progress>, ps: &Entity<Ps>) -> Div {
    let title = display_title(&offer.game);
    let title_text = match mode {
        Mode::Copy => t!("ps.add.copying", title = title),
        Mode::Move => t!("ps.add.moving", title = title),
    };
    let snapshot = progress.snapshot();
    let stopping = progress.is_cancelled();
    let text = if stopping {
        t!("ps.add.cancelling")
    } else {
        snapshot.map_or_else(
            || t!("ps.add.measuring"),
            |(done, total)| {
                t!(
                    "ps.add.progress",
                    done = crate::format::bytes(done),
                    total = crate::format::bytes(total),
                    percent = format!("{:.0}", done as f64 * 100. / total as f64)
                )
            },
        )
    };
    let cancel = ps.clone();
    div()
        .flex()
        .flex_col()
        .gap(px(6.))
        .child(heading(title_text))
        .child(progress_bar(
            "ps-add-progress",
            snapshot.map(|(done, total)| done as f64 / total as f64),
            6.,
        ))
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(10.))
                .child(div().flex_1().min_w_0().child(theme::muted(text).text_size(px(12.))))
                .child(
                    button("ps-add-stop", ButtonKind::Secondary, t!("ps.add.cancel"), stopping).when(!stopping, |b| {
                        b.on_click(move |_, _, cx| cancel.update(cx, Ps::cancel_add))
                    }),
                ),
        )
}

fn notice(text: &Text, is_error: bool, ps: &Entity<Ps>) -> Div {
    let close = ps.clone();
    let text = SharedString::from(text.clone());
    div()
        .flex()
        .items_center()
        .gap(px(10.))
        .child(div().flex_1().min_w_0().child(if is_error {
            theme::error(text)
        } else {
            theme::muted(text)
        }))
        .child(
            button("ps-add-close", ButtonKind::Icon, t!("ps.add.close"), false)
                .on_click(move |_, _, cx| close.update(cx, Ps::dismiss_add_notice)),
        )
}
