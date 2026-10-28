//! The Ports tab's card for psport's branch built on this machine ([`decanter_ps::os::windows::winbuild`]): the
//! branch's newest commit, the confirmation asked before building a commit the first time, the
//! build's progress and last lines, and the builds in the library to use or delete.

use std::borrow::Cow;

use decanter_ps::os::windows::winbuild::{self, Built, Commit, Head, Stage};
use gpui::{AnyElement, App, Div, Entity, SharedString, div, prelude::*, px, rgb};
use rust_i18n::t;

use crate::state::Ps;
use crate::theme;
use crate::widgets::{ButtonKind, button, progress_bar};

pub fn card(ps: &Entity<Ps>, cx: &App) -> AnyElement {
    let state = ps.read(cx).branch();
    let selected = ps.read(cx).package().selected_build();
    let building = state.building();
    let busy = building.is_some();
    let deleting = state.is_deleting();
    let checking = state.is_checking();
    let toolchain = if winbuild::pins::package().is_none() {
        t!("ps.branch.toolchain.not_pinned")
    } else if state.is_toolchain_installed() {
        t!(
            "ps.branch.toolchain.installed",
            number = winbuild::pins::TOOLCHAIN.number
        )
    } else {
        t!("ps.branch.toolchain.missing", number = winbuild::pins::TOOLCHAIN.number)
    };
    let head = match state.head() {
        Some(Ok(head)) => Some(t!(
            "ps.branch.head",
            branch = head.branch,
            commit = head.commit.short(),
            remote = winbuild::git::REMOTE
        )),
        Some(Err(err)) => Some(t!("ps.branch.check_error", error = err)),
        None => None,
    };
    let built_head = state
        .head()
        .and_then(|head| head.as_ref().ok())
        .filter(|head| state.built().iter().any(|built| built.commit == head.commit));
    let can_build = state
        .head()
        .and_then(|head| head.as_ref().ok())
        .filter(|_| built_head.is_none() && state.confirming().is_none())
        .cloned();
    let progress = building.map(|head| progress(head, state.stage()));
    let lines = (busy && !state.lines().is_empty()).then(|| {
        theme::mono()
            .text_size(px(11.))
            .children(state.lines().iter().map(|line| div().truncate().child(line.clone())))
    });
    let count = state.built().len();
    let list = state
        .built()
        .iter()
        .enumerate()
        .map(|(index, built)| build_row(ps, index, built, selected, busy || deleting, index + 1 == count));

    theme::card()
        .p_4()
        .gap_2()
        .child(theme::h3(t!("ps.branch.title")))
        .child(theme::muted(t!("ps.branch.description")))
        .child(theme::muted(toolchain))
        .children(head.map(|head| div().child(SharedString::from(head))))
        .children(state.confirming().map(|head| confirmation(ps, head)))
        .children(progress)
        .children(lines)
        .when(deleting, |card| card.child(theme::muted(t!("ps.branch.deleting"))))
        .children(state.error().map(theme::error))
        .child(actions(
            ps,
            checking,
            can_build.filter(|_| !busy && !deleting),
            busy,
            state.is_toolchain_installed() && !deleting,
        ))
        .when(count > 0, |card| card.child(theme::card().mt_2().children(list)))
        .into_any_element()
}

/// Consultar, Compilar or Detener, and Borrar la toolchain.
fn actions(ps: &Entity<Ps>, checking: bool, can_build: Option<Head>, busy: bool, has_toolchain: bool) -> Div {
    div()
        .flex()
        .gap_2()
        .mt_2()
        .child(
            button(
                "ps-branch-check",
                ButtonKind::Secondary,
                if checking {
                    t!("ps.ports.checking")
                } else {
                    t!("ps.branch.check")
                },
                checking,
            )
            .when(!checking, |b| {
                let ps = ps.clone();
                b.on_click(move |_, _, cx| ps.update(cx, Ps::branch_check))
            }),
        )
        .children(can_build.map(|head| {
            let ps = ps.clone();
            button(
                "ps-branch-build",
                ButtonKind::Primary,
                t!("ps.branch.build", commit = head.commit.short()),
                false,
            )
            .on_click(move |_, _, cx| ps.update(cx, |ps, cx| ps.branch_build(&head, cx)))
        }))
        .when(busy, |row| {
            let ps = ps.clone();
            row.child(
                button("ps-branch-stop", ButtonKind::Secondary, t!("ps.branch.stop"), false)
                    .on_click(move |_, _, cx| ps.update(cx, Ps::branch_stop)),
            )
        })
        .when(has_toolchain, |row| {
            let ps = ps.clone();
            row.child(
                button(
                    "ps-branch-delete-toolchain",
                    ButtonKind::Secondary,
                    t!("ps.branch.delete_toolchain"),
                    busy,
                )
                .when(!busy, |b| {
                    b.on_click(move |_, _, cx| ps.update(cx, Ps::branch_delete_toolchain))
                }),
            )
        })
}

/// What will be built and what building means, with the owner's yes or no.
fn confirmation(ps: &Entity<Ps>, head: &Head) -> AnyElement {
    let (yes, no) = (ps.clone(), ps.clone());
    div()
        .flex()
        .flex_col()
        .gap_2()
        .p_3()
        .border_1()
        .border_color(rgb(theme::ACCENT_GOLD))
        .rounded_md()
        .child(theme::h3(t!("ps.branch.confirm.title")))
        .child(theme::mono().child(t!("ps.branch.confirm.remote", remote = winbuild::git::REMOTE)))
        .child(theme::mono().child(t!("ps.branch.confirm.branch", branch = head.branch)))
        .child(theme::mono().child(t!("ps.branch.confirm.commit", commit = head.commit)))
        .child(div().child(SharedString::from(t!("ps.branch.confirm.text"))))
        .child(
            div()
                .flex()
                .gap_2()
                .child(
                    button(
                        "ps-branch-confirm",
                        ButtonKind::Primary,
                        t!("ps.branch.confirm.yes", commit = head.commit.short()),
                        false,
                    )
                    .on_click(move |_, _, cx| yes.update(cx, Ps::branch_confirm)),
                )
                .child(
                    button(
                        "ps-branch-cancel",
                        ButtonKind::Secondary,
                        t!("ps.branch.confirm.no"),
                        false,
                    )
                    .on_click(move |_, _, cx| no.update(cx, Ps::branch_cancel_confirm)),
                ),
        )
        .into_any_element()
}

fn progress(head: &Head, stage: Option<&Stage>) -> AnyElement {
    let (text, fraction): (Cow<'static, str>, Option<f64>) = match stage {
        None => (t!("ps.branch.stage.starting"), None),
        Some(Stage::Downloading { name, received, total }) => (
            t!(
                "ps.branch.stage.downloading",
                name = name,
                size = crate::format::bytes(*received)
            ),
            total.map(|total| *received as f64 / total.max(1) as f64),
        ),
        Some(Stage::Unpacking(name)) => (t!("ps.branch.stage.unpacking", name = name), None),
        Some(Stage::Verifying { checked, total }) => (
            t!("ps.branch.stage.verifying", checked = checked, total = total),
            (*total > 0).then(|| f64::from(*checked) / f64::from(*total)),
        ),
        Some(Stage::Sources) => (t!("ps.branch.stage.sources"), None),
        Some(Stage::Configuring) => (t!("ps.branch.stage.configuring"), None),
        Some(Stage::Compiling) => (t!("ps.branch.stage.compiling"), None),
        Some(Stage::Assembling) => (t!("ps.branch.stage.assembling"), None),
    };
    div()
        .flex()
        .flex_col()
        .gap_1()
        .child(theme::muted(t!(
            "ps.branch.building",
            commit = head.commit.short(),
            stage = text
        )))
        .child(progress_bar("ps-branch-progress", fraction, 4.))
        .into_any_element()
}

fn build_row(
    ps: &Entity<Ps>,
    index: usize,
    built: &Built,
    selected: Option<&Commit>,
    busy: bool,
    last: bool,
) -> AnyElement {
    let is_selected = selected == Some(&built.commit);
    let date = i64::try_from(built.built_at)
        .ok()
        .and_then(|secs| chrono::DateTime::from_timestamp(secs, 0))
        .map(|at| at.with_timezone(&chrono::Local).format("%Y-%m-%d %H:%M").to_string())
        .unwrap_or_default();
    let details = t!(
        "ps.branch.built",
        branch = built.branch,
        date = date,
        number = built.toolchain
    );
    let mut actions = div().flex().gap_2();
    if !is_selected {
        let (for_use, for_delete) = ((ps.clone(), built.commit.clone()), (ps.clone(), built.commit.clone()));
        actions = actions
            .child(
                button(
                    ("ps-branch-use", index),
                    ButtonKind::Secondary,
                    t!("ps.package.use"),
                    false,
                )
                .on_click(move |_, _, cx| {
                    let (ps, commit) = &for_use;
                    ps.update(cx, |ps, cx| ps.branch_use(commit, cx));
                }),
            )
            .child(
                button(
                    ("ps-branch-delete", index),
                    ButtonKind::Secondary,
                    t!("ps.package.delete"),
                    busy,
                )
                .when(!busy, |b| {
                    b.on_click(move |_, _, cx| {
                        let (ps, commit) = &for_delete;
                        ps.update(cx, |ps, cx| ps.branch_delete(commit, cx));
                    })
                }),
            );
    }
    theme::list_row(("ps-branch-row", index), last)
        .child(
            div()
                .flex()
                .flex_col()
                .flex_1()
                .min_w_0()
                .child(theme::mono().child(built.commit.short().to_owned()))
                .child(theme::muted(details).text_size(px(11.))),
        )
        .children(is_selected.then(|| theme::badge(t!("ps.package.badge_in_use"), true)))
        .child(actions)
        .into_any_element()
}
