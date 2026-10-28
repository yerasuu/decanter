//! The Ports tab where decanter doesn't build psport ([`decanter_ps::bbport::direct::IN_USE`]): the
//! releases that carry its Windows package and the versions in the library, to download, choose the
//! one Jugar runs and delete the others.

use decanter_ps::os::windows::package::{Installed, Offer, Version};
use gpui::{AnyElement, App, Entity, SharedString, div, prelude::*, px};
use rust_i18n::t;

use crate::state::Ps;
use crate::theme;
use crate::widgets::{ButtonKind, button, progress_bar};

/// One version, as the releases list it and the library holds it.
struct Row<'state> {
    version: &'state Version,
    offer: Option<&'state Offer>,
    installed: Option<&'state Installed>,
}

pub fn card(ps: &Entity<Ps>, cx: &App) -> AnyElement {
    let state = ps.read(cx).package();
    let selected = state.selected();
    let downloading = state.downloading();
    let checking = state.is_checking();
    let in_use = match (state.manual_folder(), state.selected_build(), selected) {
        (Some(dir), _, _) => t!("ps.package.in_use_folder", dir = dir),
        (None, Some(commit), _) => t!("ps.package.in_use_build", commit = commit.short()),
        (None, None, Some(version)) if state.is_pinned() => t!("ps.package.in_use_pinned", tag = version.tag()),
        (None, None, Some(version)) => t!("ps.package.in_use", tag = version.tag()),
        (None, None, None) => t!("ps.package.none_in_use"),
    };
    let progress = downloading.map(|(version, received, total)| {
        div()
            .flex()
            .flex_col()
            .gap_1()
            .child(theme::muted(t!(
                "ps.package.downloading",
                tag = version.tag(),
                size = crate::format::bytes(*received)
            )))
            .child(progress_bar(
                "ps-package-progress",
                total.map(|total| *received as f64 / total.max(1) as f64),
                4.,
            ))
    });
    let checked_at = state
        .checked_at()
        .filter(|_| !checking)
        .map(|at| t!("ps.ports.checked_at", time = at.format("%H:%M").to_string()));
    let offers = match state.offers() {
        Some(Ok(offers)) => offers.as_slice(),
        Some(Err(_)) | None => &[],
    };
    let listing_error = match state.offers() {
        Some(Err(err)) => Some(t!("ps.package.list_error", error = err)),
        Some(Ok(offers)) if offers.is_empty() => Some(t!("ps.package.no_releases")),
        Some(Ok(_)) | None => None,
    };
    let mut rows: Vec<Row> = offers
        .iter()
        .map(|offer| Row {
            version: &offer.version,
            offer: Some(offer),
            installed: state
                .installed()
                .iter()
                .find(|installed| installed.version == offer.version),
        })
        .collect();
    rows.extend(
        state
            .installed()
            .iter()
            .filter(|installed| !offers.iter().any(|offer| offer.version == installed.version))
            .map(|installed| Row {
                version: &installed.version,
                offer: None,
                installed: Some(installed),
            }),
    );
    rows.sort_by(|a, b| b.version.cmp(a.version));
    let count = rows.len();
    let busy = downloading.is_some();
    let list = rows
        .into_iter()
        .enumerate()
        .map(|(index, row)| version_row(ps, index, &row, selected, busy, index + 1 == count));

    theme::card()
        .p_4()
        .gap_2()
        .child(theme::h3(t!("ps.package.title")))
        .child(theme::muted(t!("ps.package.description")))
        .child(div().child(SharedString::from(in_use)))
        .children(progress)
        .children(state.error().map(theme::error))
        .children(listing_error.map(theme::muted))
        .children(checked_at.map(theme::muted))
        .child(
            div().flex().gap_2().mt_2().child(
                button(
                    "ps-package-check",
                    ButtonKind::Secondary,
                    if checking {
                        t!("ps.ports.checking")
                    } else {
                        t!("ps.ports.check")
                    },
                    checking,
                )
                .when(!checking, |b| {
                    let ps = ps.clone();
                    b.on_click(move |_, _, cx| ps.update(cx, Ps::package_check))
                }),
            ),
        )
        .when(count > 0, |card| card.child(theme::card().mt_2().children(list)))
        .into_any_element()
}

fn version_row(
    ps: &Entity<Ps>,
    index: usize,
    row: &Row,
    selected: Option<&Version>,
    busy: bool,
    last: bool,
) -> AnyElement {
    let is_selected = selected == Some(row.version);
    let mut details: Vec<String> = Vec::with_capacity(3);
    if let Some(date) = row.offer.and_then(|offer| offer.published.as_deref()) {
        details.push(t!("ps.package.published", date = date).into_owned());
    }
    if let Some(size) = row
        .offer
        .map(|offer| offer.size)
        .or_else(|| row.installed.map(|installed| installed.size))
    {
        details.push(crate::format::bytes(size));
    }
    if let Some(installed) = row.installed {
        let date = i64::try_from(installed.downloaded_at)
            .ok()
            .and_then(|secs| chrono::DateTime::from_timestamp(secs, 0))
            .map(|at| at.with_timezone(&chrono::Local).format("%Y-%m-%d").to_string())
            .unwrap_or_default();
        details.push(t!("ps.package.downloaded", date = date).into_owned());
    }
    let badge = if is_selected {
        Some(theme::badge(t!("ps.package.badge_in_use"), true))
    } else {
        row.installed
            .map(|_| theme::badge(t!("ps.package.badge_downloaded"), false))
    };
    let version = row.version.clone();
    let mut actions = div().flex().gap_2();
    if row.installed.is_none() && row.offer.is_some() {
        let (ps, version) = (ps.clone(), version.clone());
        actions = actions.child(
            button(
                ("ps-package-download", index),
                ButtonKind::Secondary,
                t!("ps.package.download"),
                busy,
            )
            .when(!busy, |b| {
                b.on_click(move |_, _, cx| ps.update(cx, |ps, cx| ps.package_download(&version, cx)))
            }),
        );
    }
    if row.installed.is_some() && !is_selected {
        let (for_use, for_delete) = ((ps.clone(), version.clone()), (ps.clone(), version));
        actions = actions
            .child(
                button(
                    ("ps-package-use", index),
                    ButtonKind::Secondary,
                    t!("ps.package.use"),
                    false,
                )
                .on_click(move |_, _, cx| {
                    let (ps, version) = &for_use;
                    ps.update(cx, |ps, cx| ps.package_use(version, cx));
                }),
            )
            .child(
                button(
                    ("ps-package-delete", index),
                    ButtonKind::Secondary,
                    t!("ps.package.delete"),
                    false,
                )
                .on_click(move |_, _, cx| {
                    let (ps, version) = &for_delete;
                    ps.update(cx, |ps, cx| ps.package_delete(version, cx));
                }),
            );
    }
    theme::list_row(("ps-package-row", index), last)
        .child(
            div()
                .flex()
                .flex_col()
                .flex_1()
                .min_w_0()
                .child(theme::mono().child(row.version.tag().to_owned()))
                .child(theme::muted(details.join(" · ")).text_size(px(11.))),
        )
        .children(badge)
        .child(actions)
        .into_any_element()
}
