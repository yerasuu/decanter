//! Fallos: crashes, STOP lines, errors and failing exits of every run in
//! this profile, newest first, each with the output around it and a report
//! ready to paste into an upstream issue.

use decanter_ps::failures::{self, Failure, Kind, Log};
use gpui::{ClipboardItem, Context, Entity, FontWeight, SharedString, Window, div, prelude::*, px, rgb};

use crate::state::Ps;
use crate::theme;
use crate::widgets::{ButtonKind, button};

pub struct FailuresView {
    ps: Entity<Ps>,
    records: Vec<Failure>,
    /// The revision `records` was read at.
    read_at: Option<u32>,
    /// The failure whose output is unfolded, by its time and message.
    expanded: Option<(u64, String)>,
    confirm_clear: bool,
    /// Shows "Copiado" until the record changes.
    copied: bool,
    error: Option<String>,
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
            copied: false,
            error: None,
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
                this.copied = false;
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
        cx.write_to_clipboard(ClipboardItem::new_string(text));
        self.copied = true;
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
                this.error = result.err().map(|err| format!("No se pudo borrar el registro: {err}"));
                this.ps.update(cx, Ps::failures_changed);
            });
        })
        .detach();
    }

    fn row(&self, index: usize, failure: &Failure, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let key = (failure.time, failure.message.clone());
        let open = self.expanded.as_ref() == Some(&key);
        let color = match failure.kind {
            Kind::Crash | Kind::Stop => theme::ERROR_TEXT,
            Kind::Error => theme::ACCENT_GOLD,
            Kind::Exit => theme::TEXT_MUTED,
            Kind::Missing => theme::ACCENT_BLUE,
        };
        let report = failure.report();
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
        let output: Vec<SharedString> = failure
            .before
            .iter()
            .chain([&failure.message])
            .chain(&failure.after)
            .map(|line| SharedString::from(line.clone()))
            .collect();
        div()
            .flex()
            .flex_col()
            .border_b_1()
            .border_color(rgb(theme::LINE))
            .child(
                div()
                    .id(("ps-failure", index))
                    .flex()
                    .items_center()
                    .gap_3()
                    .px_4()
                    .py_2()
                    .cursor_pointer()
                    .hover(|row| row.bg(rgb(theme::BG_PANEL)))
                    .child(
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
                            .child(failure.kind.label()),
                    )
                    .child(
                        theme::mono()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_size(px(12.))
                            .text_color(rgb(theme::TEXT_BRIGHT))
                            .child(failure.message.clone()),
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
                row.child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_2()
                        .px_4()
                        .pb_3()
                        .child(
                            theme::mono()
                                .p_3()
                                .bg(rgb(theme::BG_PANEL_3))
                                .border_1()
                                .border_color(rgb(theme::LINE))
                                .text_size(px(11.))
                                .children(output.into_iter().map(|line| div().child(line))),
                        )
                        .child(
                            div().flex().child(
                                button(
                                    ("ps-failure-copy", index),
                                    ButtonKind::Secondary,
                                    "Copiar informe",
                                    false,
                                )
                                .on_click(move |_, _, cx| {
                                    cx.write_to_clipboard(ClipboardItem::new_string(report.clone()));
                                }),
                            ),
                        ),
                )
            })
    }
}

impl Render for FailuresView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let empty = self.records.is_empty();
        let rows: Vec<_> = self
            .records
            .iter()
            .enumerate()
            .map(|(index, failure)| self.row(index, failure, cx).into_any_element())
            .collect();
        let clear_label = if self.confirm_clear {
            "Confirmar: borrar todo el registro"
        } else {
            "Borrar registro"
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
                        .child(theme::h2("Fallos").flex_1())
                        .when(!empty, |header| {
                            header
                                .child(
                                    button(
                                        "ps-failures-copy",
                                        ButtonKind::Secondary,
                                        if self.copied { "Copiado" } else { "Copiar todo" },
                                        false,
                                    )
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
                .child(theme::muted(
                    "Cierres, STOP, errores y salidas fallidas de cada partida de este perfil, con la salida de \
                     alrededor y el commit del port. Se guardan entre sesiones.",
                ))
                .children(self.error.clone().map(theme::error))
                .child(if empty {
                    theme::empty("Sin fallos registrados.").into_any_element()
                } else {
                    theme::card().children(rows).into_any_element()
                }),
        )
    }
}
