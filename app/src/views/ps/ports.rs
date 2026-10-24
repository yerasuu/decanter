//! Ports: where each port stands — toolchain, checkout, build, upstream —
//! and its latest clone, build or update output.
//!
//! The toolchain line is information only: which one runs a port is
//! decided by what the machine has.

use std::borrow::Cow;

use decanter_core::capabilities::Feature;
use decanter_ps::bbport::direct;
use decanter_ps::channel::Channel;
use decanter_ps::paths;
use decanter_ps::port::Port;
use decanter_ps::remote::RemoteError;
use decanter_ps::toolchain::Toolchain;
use decanter_ps::updater::{Check, Plan};
use gpui::{AnyElement, Context, Entity, SharedString, Window, div, prelude::*, px, rgb};
use rust_i18n::t;

use super::log::LogView;
use crate::capabilities::Caps;
use crate::state::{BuildOrigin, Fallback, JobKey, PortState, Ps, Stage, Track};
use crate::theme;
use crate::widgets::{ButtonKind, button, progress_bar};

pub struct PortsView {
    ps: Entity<Ps>,
    logs: Vec<(Port, Entity<LogView>)>,
    /// The port whose reset is waiting for its confirming second click.
    confirm_reset: Option<Port>,
}

impl PortsView {
    pub fn new(ps: Entity<Ps>, cx: &mut Context<Self>) -> Self {
        cx.observe(&ps, |this, _, cx| {
            this.sync_logs(cx);
            cx.notify();
        })
        .detach();
        if direct::IN_USE {
            ps.update(cx, Ps::package_reload);
            ps.update(cx, Ps::branch_reload);
        }
        let mut this = Self {
            ps,
            logs: Vec::new(),
            confirm_reset: None,
        };
        this.sync_logs(cx);
        this
    }

    /// A port's log view follows whichever job last ran for it.
    fn sync_logs(&mut self, cx: &mut Context<Self>) {
        for port in Port::ALL {
            let Some(job) = self.ps.read(cx).job(&JobKey::Port(port)).cloned() else {
                continue;
            };
            let current = self.logs.iter().position(|(logged, _)| *logged == port);
            match current {
                Some(index) if self.logs[index].1.read(cx).job() == &job => {}
                Some(index) => self.logs[index].1 = cx.new(|cx| LogView::new(job, cx)),
                None => self.logs.push((port, cx.new(|cx| LogView::new(job, cx)))),
            }
        }
    }

    fn card(&self, port: Port, cx: &mut Context<Self>) -> AnyElement {
        let ps = self.ps.read(cx);
        let state = ps.port(port);
        let stage = state.map_or(&Stage::Idle, |state| state.stage());
        let busy = state.is_some_and(|state| state.is_busy());
        let (stage_text, progress) = stage_status(stage);
        let toolchain = state
            .and_then(|state| state.toolchain())
            .map(|toolchain| t!("ps.ports.toolchain", name = toolchain_label(toolchain)));
        let origin = state.and_then(PortState::origin).map(|origin| match origin {
            BuildOrigin::Local => t!("ps.ports.origin.local"),
            BuildOrigin::Cache => t!("ps.ports.origin.cache"),
            BuildOrigin::Bundle => t!("ps.ports.origin.bundle"),
        });
        let check = state.and_then(|state| state.check());
        let upstream = check.map(upstream_text);
        let checking = state.is_some_and(PortState::is_checking);
        // Only a cloned checkout has an upstream to compare with, and only it gets a check or a track.
        let checkable = checking || check.is_some() || state.and_then(PortState::track).is_some();
        let checked_at = state
            .and_then(PortState::checked_at)
            .filter(|_| checkable && !checking)
            .map(|at| t!("ps.ports.checked_at", time = at.format("%H:%M").to_string()));
        let channel = port
            .binary_cache()
            .and(state)
            .and_then(|state| state.channel().map(|channel| channel_text(channel, state)));
        let fell_back = state
            .is_some_and(|state| state.fell_back() && state.channel() == Some(Channel::Release))
            .then(|| t!("ps.ports.channel.fell_back"));
        let needs_reset = check.is_some_and(
            |check| matches!(check, Ok(check) if matches!(check.plan, Plan::Rewritten | Plan::LocalCommits(_))),
        );
        let fallback = state.and_then(PortState::fallback).cloned();
        let failed = matches!(stage, Stage::Failed(_));
        let confirming = self.confirm_reset == Some(port);
        let log = self
            .logs
            .iter()
            .find(|(logged, _)| *logged == port)
            .map(|(_, log)| log.clone());

        theme::card()
            .p_4()
            .gap_2()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .child(theme::h3(port.name()))
                    .child(theme::muted(port.upstream()).text_size(px(11.))),
            )
            .child(
                div()
                    .text_color(rgb(if failed { theme::ERROR_TEXT } else { theme::TEXT_PRIMARY }))
                    .child(SharedString::from(stage_text)),
            )
            .children(progress.map(|fraction| progress_bar(("ps-port-progress", port as usize), Some(fraction), 4.)))
            .children(toolchain.map(theme::muted))
            .children(channel.map(theme::muted))
            .children(origin.map(theme::muted))
            .children(fell_back.map(theme::muted))
            .children(upstream.map(theme::muted))
            .children(checked_at.map(theme::muted))
            .children(fallback.map(|fallback| fallback_notice(port, fallback, busy, cx)))
            .child(
                div()
                    .flex()
                    .gap_2()
                    .mt_2()
                    .child(
                        button(
                            ("ps-port-prepare", port as usize),
                            ButtonKind::Secondary,
                            t!("ps.ports.prepare"),
                            busy,
                        )
                        .when(!busy, |b| {
                            b.on_click(cx.listener(move |this, _, _, cx| {
                                this.ps
                                    .update(cx, |ps, cx| drop(ps.prepare(port, &paths::active(), cx)));
                            }))
                        }),
                    )
                    .when(checkable, |row| {
                        row.child(
                            button(
                                ("ps-port-check", port as usize),
                                ButtonKind::Secondary,
                                if checking {
                                    t!("ps.ports.checking")
                                } else {
                                    t!("ps.ports.check")
                                },
                                checking,
                            )
                            .when(!checking, |b| {
                                b.on_click(cx.listener(move |this, _, _, cx| {
                                    this.ps.update(cx, |ps, cx| ps.check_now(port, cx));
                                }))
                            }),
                        )
                    })
                    .when(needs_reset, |row| {
                        let label = if confirming {
                            t!("ps.ports.reset.confirm")
                        } else {
                            t!("ps.ports.reset.start")
                        };
                        row.child(
                            button(("ps-port-reset", port as usize), ButtonKind::Secondary, label, busy).when(
                                !busy,
                                |b| {
                                    b.on_click(cx.listener(move |this, _, _, cx| {
                                        if this.confirm_reset == Some(port) {
                                            this.confirm_reset = None;
                                            this.ps.update(cx, |ps, cx| ps.reset_to_upstream(port, cx));
                                        } else {
                                            this.confirm_reset = Some(port);
                                        }
                                        cx.notify();
                                    }))
                                },
                            ),
                        )
                    }),
            )
            .children(log.map(|log| div().h(px(220.)).mt_2().child(log)))
            .into_any_element()
    }
}

impl Render for PortsView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let page = div().flex().flex_col().gap_4().child(theme::h2(t!("ps.ports.title")));
        // Where psport.exe runs directly there are no checkouts: its packages and its branch's builds.
        let page = if direct::IN_USE {
            let package = Caps::get(cx)
                .is_offered(Feature::PsDownloads)
                .then(|| Caps::get(cx).mark(Feature::PsDownloads, super::package::card(&self.ps, cx)));
            let branch = Caps::get(cx).gate_or_reason(Feature::PsBuilds, super::branch::card(&self.ps, cx));
            page.children(package).child(branch)
        } else if let Some(reason) = Caps::get(cx).reason(Feature::PsBuilds) {
            page.child(theme::muted(reason))
        } else {
            let cards: Vec<AnyElement> = Port::ALL.into_iter().map(|port| self.card(port, cx)).collect();
            page.child(theme::muted(t!("ps.ports.description"))).children(cards)
        };
        theme::page("ps-ports", page)
    }
}

fn stage_status(stage: &Stage) -> (Cow<'static, str>, Option<f64>) {
    match stage {
        Stage::Idle => (t!("ps.ports.stage.idle"), None),
        Stage::Resolving => (t!("ps.ports.stage.resolving"), None),
        Stage::Downloading(received, total) => (
            t!("ps.ports.stage.downloading", size = crate::format::bytes(*received)),
            total.map(|total| *received as f64 / total.max(1) as f64),
        ),
        Stage::DownloadingBundle(received, total) => (
            t!(
                "ps.ports.stage.downloading_bundle",
                size = crate::format::bytes(*received)
            ),
            total.map(|total| *received as f64 / total.max(1) as f64),
        ),
        Stage::Cloning => (t!("ps.ports.stage.cloning"), None),
        Stage::Updating => (t!("ps.ports.stage.updating"), None),
        Stage::Fetching => (t!("ps.ports.stage.fetching"), None),
        Stage::Building => (t!("ps.ports.stage.building"), None),
        Stage::RollingBack => (t!("ps.ports.stage.rolling_back"), None),
        Stage::Ready => (t!("ps.ports.stage.ready"), None),
        Stage::Failed(message) => (message.to_string().into(), None),
    }
}

/// The port runs on an earlier commit than upstream's, with the failed build's log and the
/// retry the automatic update no longer makes.
fn fallback_notice(port: Port, fallback: Fallback, busy: bool, cx: &mut Context<PortsView>) -> AnyElement {
    div()
        .flex()
        .flex_col()
        .gap_1()
        .child(div().text_color(rgb(theme::ACCENT_GOLD)).child(SharedString::from(t!(
            "ps.ports.fallback.text",
            good = fallback.good,
            failed = fallback.failed
        ))))
        .child(
            div()
                .flex()
                .items_center()
                .gap_3()
                .children(fallback.log.map(|path| {
                    div()
                        .id(("ps-port-fallback-log", port as usize))
                        .cursor_pointer()
                        .text_color(rgb(theme::ACCENT_BLUE))
                        .hover(|link| link.text_color(rgb(theme::TEXT_BRIGHT)))
                        .on_click(move |_, _, _| {
                            if let Err(err) = crate::desktop::open(&path) {
                                tracing::warn!(path = %path.display(), %err, "could not open the failed build's log");
                            }
                        })
                        .child(t!("ps.ports.fallback.log"))
                }))
                .child(
                    button(
                        ("ps-port-retry", port as usize),
                        ButtonKind::Secondary,
                        t!("ps.ports.fallback.retry"),
                        busy,
                    )
                    .when(!busy, |b| {
                        b.on_click(cx.listener(move |this, _, _, cx| {
                            this.ps.update(cx, |ps, cx| ps.retry_update(port, cx));
                        }))
                    }),
                ),
        )
        .into_any_element()
}

fn channel_text(channel: Channel, state: &PortState) -> Cow<'static, str> {
    match (channel, state.track()) {
        (Channel::Testing, _) => {
            if let Some(Ok(check)) = state.check() {
                t!("ps.ports.channel.testing_at", commit = check.head)
            } else {
                t!("ps.ports.channel.testing")
            }
        }
        (Channel::Release, Some(Track::OnRelease(release))) => t!("ps.ports.channel.release_at", tag = release.tag),
        (Channel::Release, Some(Track::Behind(release))) => t!("ps.ports.channel.behind", tag = release.tag),
        (Channel::Release, Some(Track::NoRelease)) => t!("ps.ports.channel.no_release"),
        (Channel::Release, Some(Track::Unknown(error))) => {
            t!("ps.ports.channel.unknown", error = remote_error_text(error))
        }
        (Channel::Release, None) => t!("ps.ports.channel.release"),
    }
}

fn upstream_text(check: &Result<Check, RemoteError>) -> Cow<'static, str> {
    match check {
        Ok(check) => match &check.plan {
            Plan::UpToDate => t!("ps.ports.upstream.up_to_date", head = check.head, branch = check.branch),
            Plan::FastForward if check.behind == 0 => {
                t!("ps.ports.upstream.switching", head = check.head, branch = check.branch)
            }
            Plan::FastForward if check.behind == 1 => t!(
                "ps.ports.upstream.behind.one",
                head = check.head,
                count = check.behind,
                branch = check.branch
            ),
            Plan::FastForward => t!(
                "ps.ports.upstream.behind.other",
                head = check.head,
                count = check.behind,
                branch = check.branch
            ),
            Plan::Rewritten => t!("ps.ports.upstream.rewritten", head = check.head),
            Plan::LocalCommits(1) => t!("ps.ports.upstream.local.one", head = check.head, count = 1),
            Plan::LocalCommits(count) => t!("ps.ports.upstream.local.other", head = check.head, count = count),
        },
        Err(err) => t!("ps.ports.upstream.error", error = remote_error_text(err)),
    }
}

fn remote_error_text(err: &RemoteError) -> Cow<'_, str> {
    match err {
        RemoteError::Offline(_) => t!("ps.ports.remote.offline"),
        RemoteError::RateLimited => t!("ps.ports.remote.rate_limited"),
        RemoteError::Missing(_) => t!("ps.ports.remote.missing"),
        RemoteError::Failed(detail) => Cow::Borrowed(detail),
    }
}

fn toolchain_label(toolchain: &Toolchain) -> std::borrow::Cow<'static, str> {
    match toolchain {
        Toolchain::Host => t!("ps.ports.toolchains.host"),
        Toolchain::SystemNix(_) => t!("ps.ports.toolchains.system_nix"),
        Toolchain::Portable(_) => t!("ps.ports.toolchains.portable"),
        Toolchain::Bundle => t!("ps.ports.toolchains.bundle"),
    }
}
