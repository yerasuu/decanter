//! Fallos: crashes, STOP lines, errors and failing exits of every run in
//! this profile, newest first, each with the output around it and a report
//! ready to paste into an upstream issue.

use std::path::PathBuf;

use decanter_ps::failures::{self, Call, CallReport, Failure, Kind, Log};
use gpui::{ClipboardItem, Context, Div, Entity, FontWeight, SharedString, Stateful, Window, div, prelude::*, px, rgb};
use rust_i18n::t;

use super::report::{Closed, ReportView};
use crate::i18n::Text;
use crate::report::scrub::Scrubber;
use crate::state::{Ps, failure_message};
use crate::theme;
use crate::widgets::{ButtonKind, button, tooltip};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Copied {
    All,
    One(usize),
}

pub struct FailuresView {
    ps: Entity<Ps>,
    records: Vec<Failure>,
    /// The revision `records` was read at.
    read_at: Option<u32>,
    /// The failure whose output is unfolded, by its time and message.
    expanded: Option<(u64, String)>,
    confirm_clear: bool,
    /// Shows "Copiado" on what was copied until the record changes.
    copied: Option<Copied>,
    error: Option<Text>,
    report: Option<Entity<ReportView>>,
}

impl FailuresView {
    pub fn new(ps: Entity<Ps>, cx: &mut Context<Self>) -> Self {
        cx.observe(&ps, |this, _, cx| this.reload(cx)).detach();
        let mut this = Self {
            ps,
            records: Vec::new(),
            read_at: None,
            expanded: None,
            confirm_clear: false,
            copied: None,
            error: None,
            report: None,
        };
        this.reload(cx);
        this
    }

    /// Only when the record changed: the section notifies far more often
    /// than a game fails.
    fn reload(&mut self, cx: &mut Context<Self>) {
        let ps = self.ps.read(cx);
        let revision = ps.failures_revision();
        if self.read_at == Some(revision) {
            return;
        }
        self.read_at = Some(revision);
        let Some(path) = ps.failures_path() else {
            self.records.clear();
            cx.notify();
            return;
        };
        let task = cx.background_spawn(async move { Log::load(path).records().to_vec() });
        cx.spawn(async move |this, cx| {
            let records = task.await;
            let _ = this.update(cx, |this, cx| {
                this.records = records;
                this.records.reverse();
                this.copied = None;
                cx.notify();
            });
        })
        .detach();
    }

    /// Every listed failure's report, newest first, one after another.
    fn copy_all(&mut self, cx: &mut Context<Self>) {
        let mut text = String::new();
        for report in self.records.iter().map(Failure::report) {
            if !text.is_empty() {
                text.push('\n');
            }
            text.push_str(&report);
        }
        Self::copy(text, Copied::All, cx);
    }

    /// Scrubbed off the UI thread, since collecting the machine's names reads the database and
    /// settings: a report is made to be pasted into an issue or a chat.
    fn copy(text: String, what: Copied, cx: &Context<Self>) {
        let task = cx.background_spawn(async move { Scrubber::collect().log(&text).0 });
        cx.spawn(async move |this, cx| {
            let text = task.await;
            let _ = this.update(cx, |this, cx| {
                cx.write_to_clipboard(ClipboardItem::new_string(text));
                this.copied = Some(what);
                cx.notify();
            });
        })
        .detach();
    }

    fn open_report(&mut self, failure: Failure, cx: &mut Context<Self>) {
        let panel = cx.new(|cx| ReportView::new(failure, cx));
        cx.subscribe(&panel, |this, _, _: &Closed, cx| {
            this.report = None;
            cx.notify();
        })
        .detach();
        self.report = Some(panel);
        cx.notify();
    }

    fn clear(&mut self, cx: &mut Context<Self>) {
        let Some(path) = self.ps.read(cx).failures_path() else {
            return;
        };
        let task = cx.background_spawn(async move { Log::load(path).clear_all() });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                this.error = result
                    .err()
                    .map(|err| Text::new("ps.failures.clear_error").arg("error", err));
                this.ps.update(cx, Ps::failures_changed);
            });
        })
        .detach();
    }

    fn row(&self, index: usize, failure: &Failure, last: bool, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let key = (failure.time, failure.message.clone());
        let open = self.expanded.as_ref() == Some(&key);
        let color = match failure.kind {
            Kind::Crash | Kind::Stop => theme::ERROR_TEXT,
            Kind::Error => theme::ACCENT_GOLD,
            Kind::Exit => theme::TEXT_MUTED,
            Kind::Missing => theme::ACCENT_BLUE,
        };
        let report = failure.report();
        let copy_button = button(
            ("ps-failure-copy", index),
            ButtonKind::Secondary,
            if self.copied == Some(Copied::One(index)) {
                t!("ps.failures.copied")
            } else {
                t!("ps.failures.copy_report")
            },
            false,
        )
        .tooltip(tooltip(t!("ps.copy_scrubbed")))
        .on_click(cx.listener(move |_, _, _, cx| Self::copy(report.clone(), Copied::One(index), cx)));
        let calls = failure.calls.as_ref().map(calls_lists);
        let log = failure.log.clone();
        let for_report = failure.clone();
        let report_button = button(
            ("ps-failure-report", index),
            ButtonKind::Secondary,
            t!("ps.report.open"),
            false,
        )
        .on_click(cx.listener(move |this, _, _, cx| this.open_report(for_report.clone(), cx)));
        let commit = if failure.commit.is_empty() {
            "?"
        } else {
            &failure.commit
        };
        let when = format!(
            "{} · {} · {}",
            failure.title_id,
            failures::format_time(failure.time),
            commit
        );
        let output: Vec<SharedString> = failure.output().map(|line| SharedString::from(line.clone())).collect();
        div()
            .flex()
            .flex_col()
            .when(!last, |row| row.border_b_1().border_color(rgb(theme::LINE)))
            .child(
                theme::compact_row(true)
                    .id(("ps-failure", index))
                    .cursor_pointer()
                    .hover(|row| row.bg(rgb(theme::BG_PANEL)))
                    .child(kind_badge(failure.kind, color))
                    .child(
                        theme::mono()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_size(px(12.))
                            .text_color(rgb(theme::TEXT_BRIGHT))
                            .child(SharedString::from(failure_message(failure))),
                    )
                    .when(failure.count > 1, |row| {
                        row.child(theme::muted(format!("×{}", failure.count)))
                    })
                    .child(theme::muted(when).flex_none().text_size(px(11.)))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.expanded = if this.expanded.as_ref() == Some(&key) {
                            None
                        } else {
                            Some(key.clone())
                        };
                        cx.notify();
                    })),
            )
            .when(open, |row| {
                row.child(failure_details(index, calls, output, copy_button, log, report_button))
            })
    }
}

impl Render for FailuresView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let empty = self.records.is_empty();
        let last = self.records.len().saturating_sub(1);
        let rows: Vec<_> = self
            .records
            .iter()
            .enumerate()
            .map(|(index, failure)| self.row(index, failure, index == last, cx).into_any_element())
            .collect();
        let clear_label = if self.confirm_clear {
            t!("ps.failures.clear.confirm")
        } else {
            t!("ps.failures.clear.start")
        };
        theme::page(
            "ps-failures",
            div()
                .flex()
                .flex_col()
                .gap_4()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_3()
                        .child(theme::h2(t!("ps.failures.title")).flex_1())
                        .when(!empty, |header| {
                            header
                                .child(
                                    button(
                                        "ps-failures-copy",
                                        ButtonKind::Secondary,
                                        if self.copied == Some(Copied::All) {
                                            t!("ps.failures.copied")
                                        } else {
                                            t!("ps.failures.copy_all")
                                        },
                                        false,
                                    )
                                    .tooltip(tooltip(t!("ps.copy_scrubbed")))
                                    .on_click(cx.listener(|this, _, _, cx| this.copy_all(cx))),
                                )
                                .child(
                                    button("ps-failures-clear", ButtonKind::Secondary, clear_label, false).on_click(
                                        cx.listener(|this, _, _, cx| {
                                            if this.confirm_clear {
                                                this.confirm_clear = false;
                                                this.clear(cx);
                                            } else {
                                                this.confirm_clear = true;
                                            }
                                            cx.notify();
                                        }),
                                    ),
                                )
                        }),
                )
                .child(theme::muted(t!("ps.failures.description")))
                .children(self.error.as_ref().map(theme::error))
                .children(self.report.clone())
                .child(if empty {
                    theme::empty(t!("ps.failures.empty")).into_any_element()
                } else {
                    theme::card().children(rows).into_any_element()
                }),
        )
    }
}

fn kind_badge(kind: Kind, color: u32) -> Div {
    div()
        .w(px(58.))
        .flex_none()
        .flex()
        .justify_center()
        .rounded(px(theme::RADIUS_SM))
        .border_1()
        .border_color(rgb(color))
        .text_color(rgb(color))
        .text_size(px(11.))
        .font_weight(FontWeight::SEMIBOLD)
        .child(kind_label(kind))
}

fn failure_details(
    index: usize,
    calls: Option<Div>,
    output: Vec<SharedString>,
    copy_button: Stateful<Div>,
    log: Option<PathBuf>,
    report_button: Stateful<Div>,
) -> Div {
    div()
        .flex()
        .flex_col()
        .gap_2()
        .px(px(18.))
        .pb_3()
        .children(calls)
        .child(
            theme::mono()
                .p_3()
                .bg(rgb(theme::BG_PANEL_3))
                .border_1()
                .border_color(rgb(theme::LINE))
                .rounded(px(theme::RADIUS_SM))
                .text_size(px(11.))
                .children(output.into_iter().map(|line| div().child(line))),
        )
        .child(
            div()
                .flex()
                .gap_2()
                .child(copy_button)
                .child(report_button)
                .children(log.map(|path| {
                    button(
                        ("ps-failure-log", index),
                        ButtonKind::Secondary,
                        t!("ps.log.full"),
                        false,
                    )
                    .on_click(move |_, _, _| {
                        if let Err(err) = crate::desktop::open(&path) {
                            tracing::warn!(path = %path.display(), %err, "could not open the log file");
                        }
                    })
                })),
        )
}

/// The port's lists, each most calls first, empty ones left out.
fn calls_lists(calls: &CallReport) -> Div {
    let lists = [
        (
            t!("ps.failures.calls.unimplemented", count = calls.unimplemented.len()),
            &calls.unimplemented,
        ),
        (t!("ps.failures.calls.stubs", count = calls.stubs.len()), &calls.stubs),
        (
            t!("ps.failures.calls.failed", count = calls.failed.len()),
            &calls.failed,
        ),
        (
            t!("ps.failures.calls.status", count = calls.status.len()),
            &calls.status,
        ),
        (
            t!("ps.failures.calls.offline", count = calls.offline.len()),
            &calls.offline,
        ),
    ];
    div()
        .flex()
        .flex_col()
        .gap_3()
        .children(
            lists
                .into_iter()
                .filter(|(_, list)| !list.is_empty())
                .map(|(title, list)| {
                    div()
                        .flex()
                        .flex_col()
                        .child(theme::h3(title).mb_1())
                        .children(list.iter().map(call_row))
                }),
        )
}

fn call_row(call: &Call) -> Div {
    theme::mono()
        .flex()
        .gap_3()
        .text_size(px(11.))
        .child(
            theme::muted(format!("×{}", call.count))
                .w(px(64.))
                .flex_none()
                .flex()
                .justify_end(),
        )
        .child(div().text_color(rgb(theme::TEXT_BRIGHT)).child(call.name.clone()))
        .children(
            call.error
                .map(|error| div().text_color(rgb(theme::ERROR_TEXT)).child(error.to_string())),
        )
        .children(call.nid.clone().map(theme::muted))
}

fn kind_label(kind: Kind) -> std::borrow::Cow<'static, str> {
    match kind {
        Kind::Crash => t!("ps.failures.kinds.crash"),
        Kind::Stop => t!("ps.failures.kinds.stop"),
        Kind::Error => t!("ps.failures.kinds.error"),
        Kind::Exit => t!("ps.failures.kinds.exit"),
        Kind::Missing => t!("ps.failures.kinds.missing"),
    }
}
