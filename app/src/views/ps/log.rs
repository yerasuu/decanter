//! One job's output, following its end while it runs.
//!
//! Virtualized: a job keeps up to 5000 lines and only the ones on screen
//! are laid out, so a game logging every frame costs a few rows a redraw.

use decanter_ps::metric_lines::classify;
use decanter_ps::process::{Job, State};
use gpui::{
    ClipboardItem, Context, Div, Entity, ScrollStrategy, SharedString, UniformListScrollHandle, Window, div,
    prelude::*, px, rgb, uniform_list,
};

use rust_i18n::t;

use crate::state::job_label;
use crate::theme;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
    Log,
    Metrics,
}

pub struct LogView {
    job: Entity<Job>,
    scroll: UniformListScrollHandle,
    tab: Tab,
    /// Indices into the job's lines for each tab, rebuilt when the job changes (not on every
    /// redraw): the job drops its oldest line once it is full, which shifts every index, so
    /// keeping them across changes would need the job to count what it dropped. A pass over at
    /// most 5000 lines of prefix checks is cheaper than the redraw it precedes, and the buffers
    /// are reused, so no line is copied.
    log_rows: Vec<usize>,
    metric_rows: Vec<usize>,
    /// psport prints the other digests (`Layer memory:`) without `PS_FRAME_STATS`, so only this
    /// line tells whether the FPS and timings are being measured.
    has_frame_stats: bool,
    /// Shows "Copiado" until the job writes again.
    copied: bool,
}

impl LogView {
    pub fn new(job: Entity<Job>, cx: &mut Context<Self>) -> Self {
        let scroll = UniformListScrollHandle::new();
        cx.observe(&job, |this, _, cx| {
            this.index(cx);
            this.scroll_to_end();
            this.copied = false;
            cx.notify();
        })
        .detach();
        let mut view = Self {
            job,
            scroll,
            tab: Tab::Log,
            log_rows: Vec::new(),
            metric_rows: Vec::new(),
            has_frame_stats: false,
            copied: false,
        };
        view.index(cx);
        view
    }

    fn index(&mut self, cx: &Context<Self>) {
        self.log_rows.clear();
        self.metric_rows.clear();
        self.has_frame_stats = false;
        let lines = &self.job.read(cx).lines;
        for (index, is_metric) in classify(lines.iter().map(String::as_str)).enumerate() {
            if is_metric {
                self.metric_rows.push(index);
                self.has_frame_stats |= lines[index].starts_with("Frame stats: ");
            } else {
                self.log_rows.push(index);
            }
        }
    }

    fn rows(&self) -> &[usize] {
        match self.tab {
            Tab::Log => &self.log_rows,
            Tab::Metrics => &self.metric_rows,
        }
    }

    fn scroll_to_end(&self) {
        if let Some(last) = self.rows().len().checked_sub(1) {
            self.scroll.scroll_to_item(last, ScrollStrategy::Bottom);
        }
    }

    fn tabs(&self, cx: &Context<Self>) -> Div {
        let items = [
            (Tab::Log, t!("ps.log.tab_log", count = self.log_rows.len())),
            (Tab::Metrics, t!("ps.log.tab_metrics", count = self.metric_rows.len())),
        ];
        let entity = cx.entity();
        crate::widgets::tabs("ps-log-tabs", &items, self.tab, move |tab, _, cx| {
            entity.update(cx, |this, cx| this.select(tab, cx));
        })
        .px_3()
        .pt_2()
    }

    fn select(&mut self, tab: Tab, cx: &mut Context<Self>) {
        self.tab = tab;
        self.scroll_to_end();
        cx.notify();
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
        let tabs = self.tabs(cx);
        let empty = self.tab == Tab::Metrics && self.metric_rows.is_empty();
        let no_frame_stats = self.tab == Tab::Metrics && !empty && !self.has_frame_stats;
        let count = self.rows().len();
        let log_file = job.log_file.clone();
        let view = cx.entity();
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
            .child(tabs)
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
            .when(empty || no_frame_stats, |panel| {
                panel.child(
                    div()
                        .px_3()
                        .py_2()
                        .text_size(px(11.5))
                        .text_color(rgb(theme::TEXT_MUTED))
                        .child(if empty {
                            t!("ps.log.no_metrics")
                        } else {
                            t!("ps.log.no_frame_stats")
                        }),
                )
            })
            .child(
                uniform_list("ps-log", count, move |range, _, cx| {
                    let rows = view.read(cx).rows();
                    let lines = &source.read(cx).lines;
                    range
                        .filter_map(|row| lines.get(*rows.get(row)?))
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
