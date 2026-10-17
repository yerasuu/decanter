//! One job's output, following its end while it runs.
//!
//! Virtualized: a job keeps up to 5000 lines and only the ones on screen
//! are laid out, so a game logging every frame costs a few rows a redraw.

use decanter_ps::process::{Job, State};
use gpui::{
    ClipboardItem, Context, Entity, ScrollStrategy, SharedString, UniformListScrollHandle, Window, div, prelude::*, px,
    rgb, uniform_list,
};

use rust_i18n::t;

use crate::state::job_label;
use crate::theme;

pub struct LogView {
    job: Entity<Job>,
    scroll: UniformListScrollHandle,
    /// Shows "Copiado" until the job writes again.
    copied: bool,
}

impl LogView {
    pub fn new(job: Entity<Job>, cx: &mut Context<Self>) -> Self {
        let scroll = UniformListScrollHandle::new();
        cx.observe(&job, |this, job, cx| {
            let lines = job.read(cx).lines.len();
            if lines > 0 {
                this.scroll.scroll_to_item(lines - 1, ScrollStrategy::Bottom);
            }
            this.copied = false;
            cx.notify();
        })
        .detach();
        Self {
            job,
            scroll,
            copied: false,
        }
    }

    /// The status line and every kept line, built only when asked for:
    /// a game's log can hold 5000 lines.
    fn copy(&mut self, status: &str, cx: &mut Context<Self>) {
        let job = self.job.read(cx);
        let mut text =
            String::with_capacity(job.lines.iter().map(|line| line.len() + 1).sum::<usize>() + status.len() + 1);
        text.push_str(status);
        text.push('\n');
        for line in &job.lines {
            text.push_str(line);
            text.push('\n');
        }
        cx.write_to_clipboard(ClipboardItem::new_string(text));
        self.copied = true;
        cx.notify();
    }

    pub fn job(&self) -> &Entity<Job> {
        &self.job
    }
}

impl Render for LogView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let job = self.job.read(cx);
        let label = job_label(job.kind);
        let status = SharedString::from(match job.state {
            State::Running(_) => t!("ps.log.running", label = label),
            State::Exited(Some(0)) => t!("ps.log.finished", label = label),
            State::Exited(Some(code)) => t!("ps.log.exited", label = label, code = code),
            State::Exited(None) if job.stop_requested => t!("ps.log.stopped", label = label),
            State::Exited(None) => t!("ps.log.interrupted", label = label),
        });
        let count = job.lines.len();
        let log_file = job.log_file.clone();
        let source = self.job.clone();
        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(rgb(theme::BG_PANEL_3))
            .border_1()
            .border_color(rgb(theme::LINE))
            .rounded(px(theme::RADIUS))
            .overflow_hidden()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .px_3()
                    .py_2()
                    .border_b_1()
                    .border_color(rgb(theme::LINE))
                    .text_size(px(11.))
                    .text_color(rgb(theme::TEXT_MUTED))
                    .child(div().flex_1().min_w_0().truncate().child(status.clone()))
                    .child(
                        div()
                            .id("ps-log-copy")
                            .flex_none()
                            .cursor_pointer()
                            .text_color(rgb(theme::ACCENT_BLUE))
                            .hover(|link| link.text_color(rgb(theme::TEXT_BRIGHT)))
                            .on_click(cx.listener(move |this, _, _, cx| this.copy(&status, cx)))
                            .child(if self.copied {
                                t!("ps.log.copied")
                            } else {
                                t!("ps.log.copy")
                            }),
                    )
                    .children(log_file.map(|path| {
                        div()
                            .id("ps-log-file")
                            .flex_none()
                            .cursor_pointer()
                            .text_color(rgb(theme::ACCENT_BLUE))
                            .hover(|link| link.text_color(rgb(theme::TEXT_BRIGHT)))
                            .on_click(move |_, _, _| {
                                if let Err(err) = crate::desktop::open(&path) {
                                    tracing::warn!(path = %path.display(), %err, "could not open the log file");
                                }
                            })
                            .child(t!("ps.log.full"))
                    })),
            )
            .child(
                uniform_list("ps-log", count, move |range, _, cx| {
                    let lines = &source.read(cx).lines;
                    range
                        .filter_map(|index| lines.get(index))
                        .map(|line| {
                            theme::mono()
                                .px_3()
                                .text_size(px(11.5))
                                .whitespace_nowrap()
                                .child(SharedString::from(line.clone()))
                        })
                        .collect()
                })
                .track_scroll(&self.scroll)
                .flex_1()
                .py_1(),
            )
    }
}
