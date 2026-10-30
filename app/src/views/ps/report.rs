//! «Reportar errores»: the report of one failed run as it would go out, and what the player can do
//! with it. Nothing leaves the machine until the player has pressed «Enviar informe» and confirmed;
//! there is no automatic send and no retry (decisions #1041, #1043).

use std::cmp::Reverse;
use std::path::{Path, PathBuf};

use decanter_ps::failures::{Failure, Kind};
use gpui::{ClipboardItem, Context, EventEmitter, SharedString, Window, div, prelude::*, px, rgb};
use rust_i18n::t;

use crate::i18n::Text;
use crate::report::send::{self, Deleted, Server, Submitted};
use crate::report::sent::Entry;
use crate::report::{self, Attachment, Built};
use crate::theme;
use crate::widgets::{ButtonKind, button, checkbox};

/// The failure a failed run's report is about (decision #1043): of the run that failed last in `dir`
/// the closing one if it has one, else its error, else what it lacked, the newest of each. A run is
/// told by its log file; records with none are told apart by their time and message.
pub fn primary_failure(records: &[Failure], dir: &Path) -> Option<Failure> {
    let latest = records.iter().rev().find(|record| record.game_dir == dir)?;
    let rank = |kind: Kind| match kind {
        Kind::Crash | Kind::Stop | Kind::Exit => 0,
        Kind::Error => 1,
        Kind::Missing => 2,
    };
    records
        .iter()
        .filter(|record| {
            record.game_dir == dir
                && if latest.log.is_some() {
                    record.log == latest.log
                } else {
                    record.time == latest.time && record.message == latest.message
                }
        })
        .min_by_key(|record| (rank(record.kind), Reverse(record.time)))
        .cloned()
}

/// Emitted when the player closes the panel.
pub struct Closed;

enum Build {
    Building,
    Ready(Built),
    Failed(Text),
}

enum Send {
    Idle,
    Sending,
    Failed(Text),
    /// The same failure was sent that day: the link of the earlier report.
    Duplicate(String),
    Done(Box<Done>),
}

/// The log as it would be attached, scrubbed when the player ticks it so the counts are shown
/// before anything is sent, and what is sent is exactly what was counted.
enum Prepared {
    Cleaning,
    Ready(Attachment),
    Failed(Text),
}

/// What the player is shown once a report is out: its address and delete code, and how the rest of
/// the send went.
struct Done {
    entry: Entry,
    notes: Vec<(Text, bool)>,
    deleting: bool,
    deleted: Option<Result<Text, Text>>,
    copied: Option<Copied>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Copied {
    Address,
    Code,
}

pub struct ReportView {
    failure: Failure,
    build: Build,
    saved: Option<Result<PathBuf, Text>>,
    /// `Ok(None)` when the setting turned sending off: the report can be read and saved, not sent.
    server: Result<Option<Server>, Text>,
    attach_log: bool,
    attachment: Option<Prepared>,
    confirming: bool,
    send: Send,
}

impl EventEmitter<Closed> for ReportView {}

impl ReportView {
    /// Builds the report off the UI thread: it reads the run's log and the machine's names.
    pub fn new(failure: Failure, cx: &mut Context<Self>) -> Self {
        let for_report = failure.clone();
        let task = cx.background_spawn(async move { report::psport(&for_report).map_err(|err| err.text()) });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                this.build = match result {
                    Ok(built) => Build::Ready(built),
                    Err(text) => Build::Failed(text),
                };
                cx.notify();
            });
        })
        .detach();
        Self {
            failure,
            build: Build::Building,
            saved: None,
            server: send::configured().map_err(|err| err.text()),
            attach_log: false,
            attachment: None,
            confirming: false,
            send: Send::Idle,
        }
    }

    fn save(&mut self, cx: &mut Context<Self>) {
        let Build::Ready(built) = &self.build else {
            return;
        };
        let built = built.clone();
        let task = cx.background_spawn(async move { report::save(&built, None).map_err(|err| err.text()) });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                this.saved = Some(result);
                cx.notify();
            });
        })
        .detach();
    }

    fn toggle_log(&mut self, cx: &mut Context<Self>) {
        self.attach_log = !self.attach_log;
        self.confirming = false;
        cx.notify();
        let Some(log) = self.failure.log.clone() else {
            return;
        };
        if !self.attach_log || matches!(self.attachment, Some(Prepared::Cleaning | Prepared::Ready(_))) {
            return;
        }
        self.attachment = Some(Prepared::Cleaning);
        let task = cx.background_spawn(async move { report::attachment(&log).map_err(|err| err.text()) });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                this.attachment = Some(match result {
                    Ok(attachment) => Prepared::Ready(attachment),
                    Err(text) => Prepared::Failed(text),
                });
                cx.notify();
            });
        })
        .detach();
    }

    /// The log is asked for and not scrubbed yet, or could not be: nothing is sent until it is.
    const fn is_log_pending(&self) -> bool {
        self.attach_log && self.failure.log.is_some() && !matches!(self.attachment, Some(Prepared::Ready(_)))
    }

    /// Only reached from the confirmation, so one press of «Sí, enviar» is one send.
    fn submit(&mut self, cx: &mut Context<Self>) {
        let (Build::Ready(built), Ok(Some(server))) = (&self.build, &self.server) else {
            return;
        };
        let log = match &self.attachment {
            Some(Prepared::Ready(attachment)) if self.attach_log => Some(attachment.bytes.clone()),
            _ if self.is_log_pending() => return,
            _ => None,
        };
        let (built, server) = (built.clone(), server.clone());
        self.confirming = false;
        self.send = Send::Sending;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = send::submit(&built, &server, log).await;
            let _ = this.update(cx, |this, cx| {
                this.send = match result {
                    Ok(Submitted::Duplicate(earlier)) => Send::Duplicate(earlier.url),
                    Ok(Submitted::Sent {
                        entry,
                        unrecorded,
                        attached,
                    }) => {
                        let mut notes = Vec::new();
                        if let Some(err) = unrecorded {
                            notes.push((Text::new("ps.report.send.unrecorded").text("error", err.text()), true));
                        }
                        match attached {
                            Some(Ok(())) => notes.push((Text::new("ps.report.send.log_attached"), false)),
                            Some(Err(err)) => {
                                notes.push((Text::new("ps.report.send.log_failed").text("error", err.text()), true));
                            }
                            None => {}
                        }
                        Send::Done(Box::new(Done {
                            entry,
                            notes,
                            deleting: false,
                            deleted: None,
                            copied: None,
                        }))
                    }
                    Err(err) => Send::Failed(err.text()),
                };
                cx.notify();
            });
        })
        .detach();
    }

    fn delete(&mut self, cx: &mut Context<Self>) {
        let Send::Done(done) = &mut self.send else {
            return;
        };
        let id = done.entry.id.clone();
        done.deleting = true;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = send::delete(&id).await;
            let _ = this.update(cx, |this, cx| {
                if let Send::Done(done) = &mut this.send {
                    done.deleting = false;
                    done.deleted = Some(match result {
                        Ok(Deleted::Removed) => Ok(Text::new("ps.report.send.deleted").arg("id", &id)),
                        Ok(Deleted::Gone) => Ok(Text::new("ps.report.send.gone").arg("id", &id)),
                        Err(err) => Err(err.text()),
                    });
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn copy(&mut self, what: Copied, cx: &mut Context<Self>) {
        let Send::Done(done) = &mut self.send else {
            return;
        };
        let text = match what {
            Copied::Address => done.entry.url.clone(),
            Copied::Code => done.entry.delete_code.clone(),
        };
        done.copied = Some(what);
        cx.write_to_clipboard(ClipboardItem::new_string(text));
        cx.notify();
    }

    fn sending_controls(&self, cx: &mut Context<Self>) -> gpui::Div {
        let controls = div().flex().flex_col().gap_3();
        let Ok(Some(_)) = &self.server else {
            return controls.children(self.server.as_ref().err().map(theme::error));
        };
        let idle = matches!(self.send, Send::Idle | Send::Failed(_) | Send::Duplicate(_));
        let has_log = self.failure.log.is_some();
        controls
            .when(
                has_log && matches!(self.send, Send::Idle | Send::Failed(_) | Send::Duplicate(_)),
                |controls| {
                    let checked = self.attach_log;
                    controls
                        .child(
                            div()
                                .id("ps-report-attach")
                                .flex()
                                .items_center()
                                .gap_3()
                                .cursor_pointer()
                                .on_click(cx.listener(|this, _, _, cx| this.toggle_log(cx)))
                                .child(checkbox(checked))
                                .child(div().min_w_0().child(t!("ps.report.attach_log"))),
                        )
                        .when(checked, |controls| {
                            controls.children(self.attachment.as_ref().map(|prepared| match prepared {
                                Prepared::Cleaning => theme::muted(t!("ps.report.cleaning")),
                                Prepared::Ready(attachment) => {
                                    theme::muted(attachment.cleaned.text("ps.report.cleaned"))
                                }
                                Prepared::Failed(text) => theme::error(text),
                            }))
                        })
                },
            )
            .when(idle, |controls| controls.child(theme::muted(t!("ps.report.privacy"))))
            .children(match &self.send {
                Send::Failed(text) => Some(theme::error(text)),
                Send::Duplicate(url) => Some(theme::error(Text::new("ps.report.send.duplicate").arg("url", url))),
                _ => None,
            })
            .when(matches!(self.send, Send::Sending), |controls| {
                controls.child(theme::muted(t!("ps.report.send.sending")))
            })
            .when(idle && !self.confirming, |controls| {
                controls.child(
                    div().flex().child(
                        button(
                            "ps-report-send",
                            ButtonKind::Primary,
                            t!("ps.report.send.button"),
                            self.is_log_pending(),
                        )
                        .on_click(cx.listener(|this, _, _, cx| {
                            if !this.is_log_pending() {
                                this.confirming = true;
                                cx.notify();
                            }
                        })),
                    ),
                )
            })
            .when(idle && self.confirming, |controls| {
                let question = if self.attach_log && has_log {
                    t!("ps.report.send.confirm_with_log")
                } else {
                    t!("ps.report.send.confirm")
                };
                controls.child(theme::muted(question)).child(
                    div()
                        .flex()
                        .gap_2()
                        .child(
                            button(
                                "ps-report-confirm",
                                ButtonKind::Primary,
                                t!("ps.report.send.confirm_yes"),
                                false,
                            )
                            .on_click(cx.listener(|this, _, _, cx| this.submit(cx))),
                        )
                        .child(
                            button(
                                "ps-report-cancel",
                                ButtonKind::Secondary,
                                t!("ps.report.send.cancel"),
                                false,
                            )
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.confirming = false;
                                cx.notify();
                            })),
                        ),
                )
            })
    }

    fn done_controls(done: &Done, cx: &mut Context<Self>) -> gpui::Div {
        let notes = done.notes.iter().map(
            |(text, error)| {
                if *error { theme::error(text) } else { theme::muted(text) }
            },
        );
        let controls = div().flex().flex_col().gap_3();
        if let Some(deleted) = &done.deleted {
            return controls.child(match deleted {
                Ok(text) => theme::muted(text),
                Err(text) => theme::error(text),
            });
        }
        let url = done.entry.url.clone();
        let copied = |what| done.copied == Some(what);
        let copy_label = |what, key: &'static str| {
            if copied(what) {
                t!("ps.report.send.copied")
            } else {
                t!(key)
            }
        };
        controls
            .children(notes)
            .child(theme::muted(t!("ps.report.send.address_label")))
            .child(
                theme::mono()
                    .text_size(px(12.))
                    .text_color(rgb(theme::TEXT_BRIGHT))
                    .child(SharedString::from(done.entry.url.clone())),
            )
            .child(theme::muted(t!("ps.report.send.code_label")))
            .child(
                theme::mono()
                    .text_size(px(12.))
                    .text_color(rgb(theme::TEXT_BRIGHT))
                    .child(SharedString::from(done.entry.delete_code.clone())),
            )
            .child(
                div()
                    .flex()
                    .gap_2()
                    .child(
                        button(
                            "ps-report-copy-address",
                            ButtonKind::Secondary,
                            copy_label(Copied::Address, "ps.report.send.copy"),
                            false,
                        )
                        .on_click(cx.listener(|this, _, _, cx| this.copy(Copied::Address, cx))),
                    )
                    .child(
                        button(
                            "ps-report-copy-code",
                            ButtonKind::Secondary,
                            copy_label(Copied::Code, "ps.report.send.copy_code"),
                            false,
                        )
                        .on_click(cx.listener(|this, _, _, cx| this.copy(Copied::Code, cx))),
                    )
                    .child(
                        button(
                            "ps-report-open",
                            ButtonKind::Secondary,
                            t!("ps.report.send.open_page"),
                            false,
                        )
                        .on_click(move |_, _, _| {
                            if let Err(err) = crate::desktop::open(&url) {
                                tracing::warn!(%err, "could not open the sent report's page");
                            }
                        }),
                    )
                    .child(if done.deleting {
                        button(
                            "ps-report-delete",
                            ButtonKind::Secondary,
                            t!("ps.report.send.deleting"),
                            true,
                        )
                    } else {
                        button(
                            "ps-report-delete",
                            ButtonKind::Secondary,
                            t!("ps.report.send.delete"),
                            false,
                        )
                        .on_click(cx.listener(|this, _, _, cx| this.delete(cx)))
                    }),
            )
    }
}

impl Render for ReportView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let body = match &self.build {
            Build::Building => theme::muted(t!("ps.report.building")).into_any_element(),
            Build::Failed(text) => theme::error(text).into_any_element(),
            Build::Ready(built) => theme::mono()
                .id("ps-report-json")
                .max_h(px(360.))
                .overflow_y_scroll()
                .p_3()
                .bg(rgb(theme::BG_PANEL_3))
                .border_1()
                .border_color(rgb(theme::LINE))
                .rounded(px(theme::RADIUS_SM))
                .text_size(px(11.))
                .children(
                    built
                        .json
                        .lines()
                        .map(|line| div().child(SharedString::from(line.to_owned()))),
                )
                .into_any_element(),
        };
        let ready = matches!(self.build, Build::Ready(_));
        let done = match &self.send {
            Send::Done(done) => Some(Self::done_controls(done, cx)),
            _ => None,
        };
        theme::card()
            .p_4()
            .flex()
            .flex_col()
            .gap_3()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .child(theme::h3(t!("ps.report.title")).flex_1())
                    .child(
                        button("ps-report-close", ButtonKind::Secondary, t!("ps.report.close"), false)
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(Closed))),
                    ),
            )
            .child(theme::muted(t!("ps.report.note")))
            .child(body)
            .children(self.saved.as_ref().map(|saved| match saved {
                Ok(path) => theme::muted(Text::new("ps.report.saved").arg("path", path.display())),
                Err(text) => theme::error(text),
            }))
            .when(ready, |card| {
                card.child(
                    div().flex().child(
                        button("ps-report-save", ButtonKind::Secondary, t!("ps.report.save"), false)
                            .on_click(cx.listener(|this, _, _, cx| this.save(cx))),
                    ),
                )
                .child(match done {
                    Some(done) => done,
                    None => self.sending_controls(cx),
                })
            })
    }
}
