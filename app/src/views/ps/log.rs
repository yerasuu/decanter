//! One job's output, following its end while it runs.
//!
//! Virtualized: a job keeps up to 5000 lines and only the ones on screen
//! are laid out, so a game logging every frame costs a few rows a redraw.

use decanter_ps::process::{Job, State};
use gpui::{
    Context, Entity, ScrollStrategy, SharedString, UniformListScrollHandle, Window, div, prelude::*, px, rgb,
    uniform_list,
};

use crate::theme;

pub struct LogView {
    job: Entity<Job>,
    scroll: UniformListScrollHandle,
}

impl LogView {
    pub fn new(job: Entity<Job>, cx: &mut Context<Self>) -> Self {
        let scroll = UniformListScrollHandle::new();
        cx.observe(&job, |this, job, cx| {
            let lines = job.read(cx).lines.len();
            if lines > 0 {
                this.scroll.scroll_to_item(lines - 1, ScrollStrategy::Bottom);
            }
            cx.notify();
        })
        .detach();
        Self { job, scroll }
    }

    pub fn job(&self) -> &Entity<Job> {
        &self.job
    }
}

impl Render for LogView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let job = self.job.read(cx);
        let status = match job.state {
            State::Running(_) => format!("{}: en curso", job.label),
            State::Exited(Some(0)) => format!("{}: terminado", job.label),
            State::Exited(Some(code)) => format!("{}: terminó con código {code}", job.label),
            State::Exited(None) if job.stop_requested => format!("{}: detenido", job.label),
            State::Exited(None) => format!("{}: interrumpido", job.label),
        };
        let count = job.lines.len();
        let source = self.job.clone();
        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(rgb(theme::BG_PANEL_3))
            .border_1()
            .border_color(rgb(theme::LINE))
            .rounded(px(theme::RADIUS_SM))
            .child(
                div()
                    .px_3()
                    .py_2()
                    .border_b_1()
                    .border_color(rgb(theme::LINE))
                    .text_size(px(11.))
                    .text_color(rgb(theme::TEXT_MUTED))
                    .child(status),
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
