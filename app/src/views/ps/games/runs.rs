//! Registro's measured runs: the switch that has the next runs of the game measured
//! (`PS_FRAME_STATS=1`), the one that has them start in debug mode (`PS_DEBUG=1`), and what each
//! measured run's log says about its frame rate.

use std::io;
use std::path::PathBuf;

use decanter_ps::bbport::LauncherSettings;
use decanter_ps::frame_stats::RunStats;
use decanter_ps::process::{Job, JobKind};
use decanter_ps::run_log;
use gpui::{AnyElement, Bounds, Context, Entity, PathBuilder, Pixels, Window, canvas, div, point, prelude::*, px, rgb};
use rust_i18n::t;

use crate::i18n::Text;
use crate::state::{JobKey, Ps, job_target};
use crate::theme;
use crate::widgets::{checkbox, stat_card};

/// The launch settings' key the port reads as `PS_FRAME_STATS`, here set for one title.
const FRAME_STATS: &str = "frame_stats";
/// Measured runs shown; one more is read so the oldest shown has one to compare with.
const SHOWN: usize = 5;
/// Points of a run's chart: a long session's windows are averaged down to this many.
const CHART_POINTS: usize = 120;
const CHART_HEIGHT: f32 = 44.;

struct Run {
    stats: RunStats,
    average: f64,
    /// Average FPS change from the previous measured run, in percent.
    delta: Option<f64>,
    points: Vec<f64>,
}

pub struct RunsView {
    ps: Entity<Ps>,
    dir: PathBuf,
    title_id: String,
    /// `None` until the settings are read.
    measure: Option<bool>,
    /// `None` until the settings are read.
    debug: Option<bool>,
    /// Newest first; `None` until the logs are read.
    runs: Option<Vec<Run>>,
    /// The top of every chart, so runs compare at a glance.
    scale: f64,
    error: Option<Text>,
    /// The game's job last seen and whether it ran: a run is read once it has ended.
    seen: Option<(Entity<Job>, bool)>,
}

impl RunsView {
    pub fn new(ps: Entity<Ps>, dir: PathBuf, title_id: String, cx: &mut Context<Self>) -> Self {
        cx.observe(&ps, |this, _, cx| this.sync_job(cx)).detach();
        let key = title_id.clone();
        let load = cx.background_spawn(async move {
            let settings = LauncherSettings::load(&decanter_ps::paths::active());
            (
                settings.title_flag(&key, FRAME_STATS),
                settings.title_flag(&key, crate::debug_logs::TITLE_KEY),
            )
        });
        cx.spawn(async move |this, cx| {
            let (measure, debug) = load.await;
            let _ = this.update(cx, |this, cx| {
                this.measure = Some(measure);
                this.debug = Some(debug);
                cx.notify();
            });
        })
        .detach();
        let mut this = Self {
            ps,
            dir,
            title_id,
            measure: None,
            debug: None,
            runs: None,
            scale: 0.0,
            error: None,
            seen: None,
        };
        this.seen = this.current_job(cx);
        this.reload(cx);
        this
    }

    fn current_job(&self, cx: &Context<Self>) -> Option<(Entity<Job>, bool)> {
        let job = self.ps.read(cx).job(&JobKey::Game(self.dir.clone()))?.clone();
        let running = job.read(cx).is_running();
        Some((job, running))
    }

    /// `Ps` notifies once more after a job's log is finished, which is when its run is read.
    fn sync_job(&mut self, cx: &mut Context<Self>) {
        let now = self.current_job(cx);
        if self.seen == now {
            return;
        }
        let ended = now.as_ref().is_some_and(|(_, running)| !running);
        self.seen = now;
        if ended {
            self.reload(cx);
        }
    }

    fn reload(&self, cx: &mut Context<Self>) {
        let target = job_target(&JobKey::Game(self.dir.clone()));
        let task = cx.background_spawn(async move { measured_runs(&target) });
        cx.spawn(async move |this, cx| {
            let read = task.await;
            let _ = this.update(cx, |this, cx| {
                match read {
                    Ok(runs) => {
                        this.scale = runs
                            .iter()
                            .flat_map(|run| run.points.iter().copied())
                            .fold(0.0, f64::max);
                        this.runs = Some(runs);
                    }
                    Err(err) => {
                        tracing::warn!(title_id = %this.title_id, %err, "could not list the game's run logs");
                        this.error = Some(Text::new("ps.runs.read_error").arg("error", err));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn switch(&mut self, key: &str) -> &mut Option<bool> {
        if key == FRAME_STATS {
            &mut self.measure
        } else {
            &mut self.debug
        }
    }

    fn set_switch(&mut self, key: &'static str, on: bool, cx: &mut Context<Self>) {
        *self.switch(key) = Some(on);
        self.error = None;
        let title_id = self.title_id.clone();
        let task = cx.background_spawn(async move {
            LauncherSettings::edit(&decanter_ps::paths::active(), |settings| {
                settings.set_title_flag(&title_id, key, on);
            })
        });
        cx.spawn(async move |this, cx| {
            let saved = task.await;
            let _ = this.update(cx, |this, cx| {
                if let Err(err) = saved {
                    tracing::warn!(title_id = %this.title_id, key, %err, "could not save the game's switch");
                    *this.switch(key) = Some(!on);
                    this.error = Some(Text::new("ps.runs.save_error").arg("error", err));
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn run(&self, index: usize, run: &Run) -> AnyElement {
        let stats = &run.stats;
        let value = |figure: Option<f64>| figure.map_or_else(|| "—".to_owned(), |figure| format!("{figure:.1}"));
        let range = stats
            .frame_time_range()
            .map_or_else(|| "—".to_owned(), |(min, max)| format!("{min:.1}–{max:.1} ms"));
        let (compiles, compile_ms) = stats.compiles();
        let pipelines = stats.pipelines;
        let delta = run.delta.map(|delta| {
            div()
                .text_size(px(12.))
                .text_color(rgb(if delta >= 0.0 {
                    theme::ACCENT_GREEN
                } else {
                    theme::ACCENT_GOLD
                }))
                .child(t!("ps.runs.delta", delta = format!("{delta:+.1}")))
        });
        div()
            .id(("ps-run", index))
            .flex()
            .flex_col()
            .gap_3()
            .px_4()
            .py_3()
            .when(index > 0, |row| row.border_t_1().border_color(rgb(theme::LINE)))
            .child(
                div()
                    .flex()
                    .items_baseline()
                    .gap_3()
                    .child(
                        theme::mono()
                            .text_color(rgb(theme::TEXT_PRIMARY))
                            .child(stats.started.clone().unwrap_or_else(|| "—".to_owned())),
                    )
                    .child(theme::muted(t!("ps.runs.samples", count = stats.samples.len())).text_size(px(12.)))
                    .children(delta),
            )
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .gap_2()
                    .child(stat_card(format!("{:.1}", run.average), &t!("ps.runs.average")))
                    .child(stat_card(value(stats.one_percent_low()), &t!("ps.runs.low")))
                    .child(stat_card(range, &t!("ps.runs.frame_time")))
                    .child(stat_card(
                        format!("{compiles} · {compile_ms:.0} ms"),
                        &t!("ps.runs.compiles"),
                    )),
            )
            .child(chart(run.points.clone(), self.scale, run.average))
            .child(
                theme::muted(t!(
                    "ps.runs.pipelines",
                    background = pipelines.background,
                    waited = pipelines.waited,
                    skipped = pipelines.skipped
                ))
                .text_size(px(12.)),
            )
            .into_any_element()
    }
}

/// The game's kept logs, newest first, measured ones only, each with its figures and its change
/// from the measured run before it.
fn measured_runs(target: &str) -> io::Result<Vec<Run>> {
    let dir = run_log::dir(&decanter_ps::paths::active());
    let mut read = Vec::with_capacity(SHOWN + 1);
    for path in run_log::kept(&dir, target, JobKind::Game)? {
        match RunStats::read(&path) {
            Ok(Some(stats)) => {
                if let Some(average) = stats.average_fps() {
                    read.push((stats, average));
                }
            }
            Ok(None) => {}
            Err(err) => tracing::warn!(log = %path.display(), %err, "could not read a run's log"),
        }
        if read.len() > SHOWN {
            break;
        }
    }
    let mut runs = Vec::with_capacity(SHOWN);
    let mut read = read.into_iter().peekable();
    while runs.len() < SHOWN
        && let Some((stats, average)) = read.next()
    {
        let previous = read.peek().map(|(_, previous)| *previous);
        runs.push(Run {
            points: chart_points(&stats),
            delta: previous
                .filter(|&previous| previous > 0.0)
                .map(|previous| (average - previous) / previous * 100.0),
            stats,
            average,
        });
    }
    Ok(runs)
}

/// The windows' FPS, averaged in equal groups down to [`CHART_POINTS`].
fn chart_points(stats: &RunStats) -> Vec<f64> {
    let group = stats.samples.len().div_ceil(CHART_POINTS).max(1);
    stats
        .samples
        .chunks(group)
        .map(|chunk| chunk.iter().map(|sample| sample.fps).sum::<f64>() / chunk.len() as f64)
        .collect()
}

/// FPS over the run, 0 at the bottom and `scale` at the top, with its average dashed.
fn chart(points: Vec<f64>, scale: f64, average: f64) -> impl IntoElement {
    canvas(
        |_, _, _| {},
        move |bounds, (), window, _| {
            if scale <= 0.0 {
                return;
            }
            let at = |index: usize, fps: f64| {
                let last = points.len().saturating_sub(1).max(1);
                point(
                    bounds.origin.x + bounds.size.width * (index as f32 / last as f32),
                    y_of(bounds, fps / scale),
                )
            };
            let mut line = PathBuilder::stroke(px(1.5));
            for (index, &fps) in points.iter().enumerate() {
                if index == 0 {
                    line.move_to(at(0, fps));
                } else {
                    line.line_to(at(index, fps));
                }
            }
            if let [only] = points.as_slice() {
                line.line_to(at(1, *only));
            }
            let mut mean = PathBuilder::stroke(px(1.)).dash_array(&[px(3.), px(3.)]);
            mean.move_to(point(bounds.origin.x, y_of(bounds, average / scale)));
            mean.line_to(point(
                bounds.origin.x + bounds.size.width,
                y_of(bounds, average / scale),
            ));
            if let Ok(mean) = mean.build() {
                window.paint_path(mean, rgb(theme::TEXT_DIM));
            }
            if let Ok(line) = line.build() {
                window.paint_path(line, rgb(theme::ACCENT_BLUE));
            }
        },
    )
    .w_full()
    .h(px(CHART_HEIGHT))
}

/// A fraction of the chart's height from its bottom, inset so a stroke at either end stays whole.
fn y_of(bounds: Bounds<Pixels>, fraction: f64) -> Pixels {
    let inner = (bounds.size.height - px(2.)).max(px(0.));
    bounds.origin.y + px(1.) + inner * (1.0 - fraction.clamp(0.0, 1.0) as f32)
}

impl Render for RunsView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let toggle = |id: &'static str, key: &'static str, state: Option<bool>, label, note| {
            div()
                .id(id)
                .flex()
                .items_center()
                .gap_3()
                .cursor_pointer()
                .when_some(state, |row, on| {
                    row.on_click(cx.listener(move |this, _, _, cx| this.set_switch(key, !on, cx)))
                })
                .child(checkbox(state.unwrap_or(false)))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .child(label)
                        .child(theme::muted(note).text_size(px(12.))),
                )
        };
        let measure = toggle(
            "ps-runs-measure",
            FRAME_STATS,
            self.measure,
            t!("ps.runs.measure"),
            t!("ps.runs.measure_note"),
        );
        let debug = toggle(
            "ps-runs-debug",
            crate::debug_logs::TITLE_KEY,
            self.debug,
            t!("ps.runs.debug"),
            t!("ps.runs.debug_note"),
        );
        let body = match &self.runs {
            None => theme::muted(t!("ps.runs.reading")).into_any_element(),
            Some(runs) if runs.is_empty() => theme::muted(t!("ps.runs.none")).into_any_element(),
            Some(runs) => div()
                .flex()
                .flex_col()
                .gap_2()
                .child(theme::card().children(runs.iter().enumerate().map(|(index, run)| self.run(index, run))))
                .child(
                    theme::muted(t!("ps.runs.method"))
                        .text_size(px(11.))
                        .text_color(rgb(theme::TEXT_DIM)),
                )
                .into_any_element(),
        };
        div()
            .flex()
            .flex_col()
            .gap_3()
            .child(measure)
            .child(debug)
            .children(self.error.as_ref().map(theme::error))
            .child(body)
    }
}
