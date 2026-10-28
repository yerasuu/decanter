//! The Raven section (decision #1069): Raven's build, built here only after the player confirms
//! what it downloads and takes, and the Windows programs that run with it, added with a file
//! picker or dropped on the section. Experimental, and says so.

use std::borrow::Cow;
use std::path::PathBuf;

use decanter_core::InstalledGame;
use decanter_core::runner::{Progress, Runner};
use decanter_ps::process::{Job, State};
use decanter_runtime::runner;
use gpui::{AnyElement, App, Context, Div, Entity, SharedString, Window, div, prelude::*, px, rgb};
use rust_i18n::t;

use crate::runners;
use crate::state::{AppState, Raven};
use crate::theme;
use crate::widgets::{ButtonKind, button, confirm_panel, progress_bar};

/// The end of a failed run's output shown under its program; the whole run is in its log.
const SHOWN_RUN_LINES: usize = 8;

const REPOSITORY: &str = "github.com/mirkobrombin/raven";

pub struct RavenView {
    raven: Entity<Raven>,
}

impl RavenView {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let raven = Raven::get(cx);
        cx.observe(&raven, |_, _, cx| cx.notify()).detach();
        cx.observe(&AppState::get(cx).background.clone(), |_, _, cx| cx.notify())
            .detach();
        Self { raven }
    }

    fn pick(&self, cx: &mut Context<Self>) {
        let raven = self.raven.clone();
        cx.spawn(async move |_, cx| {
            let Some(file) = rfd::AsyncFileDialog::new()
                .set_title(t!("raven.programs.pick_title"))
                .add_filter(t!("raven.programs.filter"), &["exe", "EXE"])
                .pick_file()
                .await
            else {
                return;
            };
            let path = file.path().to_path_buf();
            raven.update(cx, |raven, cx| raven.add(vec![path], cx));
        })
        .detach();
    }
}

impl Render for RavenView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let header = div()
            .flex()
            .items_center()
            .gap_3()
            .child(theme::h1("Raven"))
            .child(theme::badge(t!("raven.experimental"), false));
        let column = div()
            .flex()
            .flex_col()
            .gap_4()
            .child(header)
            .child(theme::muted(t!("raven.description")))
            .child(theme::muted(t!("raven.license")).text_size(px(11.5)))
            .child(build_card(&self.raven, cx))
            .child(self.programs_card(cx));
        theme::page("raven-page", column)
    }
}

/// Not built, building with its progress and Cancelar, built with its commit and size, or why the
/// last build failed with the end of its log.
fn build_card(raven: &Entity<Raven>, cx: &App) -> AnyElement {
    let state = raven.read(cx);
    let building = state.is_building();
    let removing = state.is_removing();
    let status: Div = if building {
        progress(state.progress())
    } else if removing {
        theme::muted(t!("raven.build.removing"))
    } else {
        match state.built() {
            Some(built) => {
                let commit = built.commit.short().to_owned();
                div().child(SharedString::from(match built.size {
                    Some(size) => t!("raven.build.built", commit = commit, size = crate::format::bytes(size)),
                    None => t!("raven.build.built_measuring", commit = commit),
                }))
            }
            None => div().child(t!("raven.build.absent")),
        }
    };
    let tail = (!state.log_tail().is_empty()).then(|| {
        theme::mono()
            .text_size(px(11.))
            .children(state.log_tail().iter().map(|line| div().truncate().child(line.clone())))
    });
    let log = Raven::log_path().filter(|path| path.is_file());
    let mut actions = div().flex().gap_2().mt_2();
    if building {
        let raven = raven.clone();
        actions = actions.child(
            button("raven-cancel", ButtonKind::Secondary, t!("raven.build.cancel"), false)
                .on_click(move |_, _, cx| raven.update(cx, Raven::stop_build)),
        );
    } else if state.built().is_some() {
        let raven = raven.clone();
        actions = actions.child(
            button(
                "raven-remove",
                ButtonKind::Secondary,
                t!("raven.build.remove"),
                removing || state.is_confirming_remove(),
            )
            .when(!removing && !state.is_confirming_remove(), |b| {
                b.on_click(move |_, _, cx| raven.update(cx, Raven::ask_remove))
            }),
        );
    } else if !state.is_confirming() {
        let raven = raven.clone();
        actions = actions.child(
            button("raven-build", ButtonKind::Primary, t!("raven.build.build"), removing).when(!removing, |b| {
                b.on_click(move |_, _, cx| raven.update(cx, Raven::ask_build))
            }),
        );
    }
    if let Some(log) = log {
        actions = actions.child(open_button("raven-build-log", t!("raven.build.open_log"), log));
    }
    theme::card()
        .p_4()
        .gap_2()
        .child(theme::h3(t!("raven.build.title")))
        .child(status)
        .children(state.is_confirming().then(|| confirmation(raven)))
        .children(
            state
                .built()
                .filter(|_| state.is_confirming_remove())
                .map(|built| remove_confirmation(raven, built.size)),
        )
        .children(state.error().map(theme::error))
        .children(tail)
        .child(actions)
        .into_any_element()
}

/// What building downloads, from where and what it takes, with the player's yes or no.
fn confirmation(raven: &Entity<Raven>) -> AnyElement {
    let (yes, no) = (raven.clone(), raven.clone());
    let commit = runner::RAVEN_COMMIT;
    div()
        .flex()
        .flex_col()
        .gap_2()
        .p_3()
        .border_1()
        .border_color(rgb(theme::ACCENT_GOLD))
        .rounded_md()
        .child(theme::h3(t!("raven.confirm.title")))
        .child(theme::mono().child(t!("raven.confirm.repo", repo = REPOSITORY)))
        .child(theme::mono().child(t!("raven.confirm.commit", commit = commit)))
        .child(div().child(SharedString::from(t!("raven.confirm.text"))))
        .child(
            div()
                .flex()
                .gap_2()
                .child(
                    button("raven-confirm", ButtonKind::Primary, t!("raven.confirm.yes"), false)
                        .on_click(move |_, _, cx| yes.update(cx, Raven::confirm_build)),
                )
                .child(
                    button("raven-confirm-no", ButtonKind::Secondary, t!("raven.confirm.no"), false)
                        .on_click(move |_, _, cx| no.update(cx, Raven::cancel_confirm)),
                ),
        )
        .into_any_element()
}

/// What removing takes away, with the size when it is measured.
fn remove_confirmation(raven: &Entity<Raven>, size: Option<u64>) -> AnyElement {
    let (yes, no) = (raven.clone(), raven.clone());
    let body = match size {
        Some(size) => t!("raven.confirm_remove.body_size", size = crate::format::bytes(size)),
        None => t!("raven.confirm_remove.body"),
    };
    confirm_panel(
        "raven-confirm-remove",
        t!("raven.confirm_remove.title"),
        body,
        t!("raven.confirm_remove.confirm"),
        move |_, _, cx| yes.update(cx, Raven::remove_build),
        move |_, _, cx| no.update(cx, Raven::cancel_remove),
    )
    .into_any_element()
}

fn progress(progress: Option<&Progress>) -> Div {
    let (text, fraction) = progress.map_or_else(
        || (t!("raven.build.stage.starting"), None),
        |progress| runners::progress(Runner::Raven, progress),
    );
    div()
        .flex()
        .flex_col()
        .gap_1()
        .child(theme::muted(text))
        .child(progress_bar("raven-progress", fraction, 4.))
}

impl RavenView {
    fn programs_card(&self, cx: &mut Context<Self>) -> AnyElement {
        let state = self.raven.read(cx);
        let is_built = state.built().is_some() && !state.is_building() && !state.is_removing();
        let profile_id = state.profile_id().cloned();
        let background = AppState::get(cx).background.read(cx);
        let count = state.programs().len();
        let rows: Vec<AnyElement> = state
            .programs()
            .iter()
            .enumerate()
            .map(|(index, program)| {
                let job = background.runs.get(&program.game_id).map(|job| job.read(cx));
                program_row(
                    &self.raven,
                    index,
                    program,
                    background.running.contains(&program.game_id),
                    is_built,
                    job,
                    profile_id.as_deref(),
                    index + 1 == count,
                )
            })
            .collect();
        let notice = state.notice().map(|(lines, is_error)| {
            let lines = lines.iter().map(|line| div().child(SharedString::from(line)));
            if *is_error {
                theme::error("").children(lines)
            } else {
                theme::muted("").children(lines)
            }
        });
        let add = button("raven-add", ButtonKind::Secondary, t!("raven.programs.add"), false)
            .on_click(cx.listener(|this, _, _, cx| this.pick(cx)));
        theme::card()
            .p_4()
            .gap_2()
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(theme::h3(t!("raven.programs.title")))
                    .child(add),
            )
            .child(theme::muted(t!("raven.programs.drop_hint")))
            .children(notice)
            .when(count == 0, |card| card.child(theme::empty(t!("raven.programs.empty"))))
            .when(count > 0, |card| card.child(theme::card().children(rows)))
            .into_any_element()
    }
}

/// One program: Ejecutar or Detener, Quitar, its run log, and the end of a run that failed.
#[expect(
    clippy::too_many_arguments,
    reason = "a row reads what the card already holds, rather than reading it again per row"
)]
fn program_row(
    raven: &Entity<Raven>,
    index: usize,
    program: &InstalledGame,
    is_running: bool,
    is_built: bool,
    job: Option<&Job>,
    profile_id: Option<&str>,
    last: bool,
) -> AnyElement {
    let path = program.launch_executable.clone().unwrap_or_default();
    let mut actions = div().flex().gap_2();
    match (profile_id, is_running) {
        (Some(profile), true) => {
            let (profile, id) = (profile.to_owned(), program.game_id.clone());
            actions = actions.child(
                button(
                    ("raven-stop", index),
                    ButtonKind::Secondary,
                    t!("raven.programs.stop"),
                    false,
                )
                .on_click(move |_, _, cx| {
                    let background = AppState::get(cx).background.clone();
                    let (profile, id) = (profile.clone(), id.clone());
                    cx.spawn(async move |cx| crate::launch::stop(profile, id, background, cx).await)
                        .detach();
                }),
            );
        }
        (Some(profile), false) => {
            let (profile, program) = (profile.to_owned(), program.clone());
            actions = actions.child(
                button(
                    ("raven-run", index),
                    ButtonKind::Primary,
                    t!("raven.programs.run"),
                    !is_built,
                )
                .when(is_built, |b| {
                    b.on_click(move |_, _, cx| {
                        let background = AppState::get(cx).background.clone();
                        let (profile, program) = (profile.clone(), program.clone());
                        tracing::info!(id = %program.game_id, "running a program with Raven");
                        cx.spawn(async move |cx| crate::launch::start(profile, program, background, cx).await)
                            .detach();
                    })
                }),
            );
        }
        (None, _) => {}
    }
    if let Some(log) = job.and_then(|job| job.log_file.clone()) {
        actions = actions.child(open_button(
            ("raven-run-log", index),
            t!("raven.programs.open_log"),
            log,
        ));
    }
    let id = program.game_id.clone();
    let remove = raven.clone();
    actions = actions.child(
        button(
            ("raven-forget", index),
            ButtonKind::Icon,
            t!("raven.programs.remove"),
            is_running,
        )
        .when(!is_running, |b| {
            b.on_click(move |_, _, cx| remove.update(cx, |raven, cx| raven.remove_program(&id, cx)))
        }),
    );
    let failed =
        job.filter(|job| !job.stop_requested && !matches!(job.state, State::Running(_) | State::Exited(Some(0))));
    let output = failed.map(|job| {
        let skipped = job.lines.len().saturating_sub(SHOWN_RUN_LINES);
        theme::error("").child(div().child(t!("raven.programs.failed"))).child(
            theme::mono().text_size(px(11.)).children(
                job.lines
                    .iter()
                    .skip(skipped)
                    .map(|line| div().truncate().child(line.clone())),
            ),
        )
    });
    div()
        .flex()
        .flex_col()
        .child(
            theme::list_row(("raven-program", index), last && output.is_none())
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .flex_1()
                        .min_w_0()
                        .child(div().truncate().child(program.name.clone()))
                        .child(theme::muted(path).text_size(px(11.)).truncate()),
                )
                .when(!is_built && !is_running, |row| {
                    row.child(theme::muted(t!("raven.programs.needs_build")).text_size(px(11.)))
                })
                .child(actions),
        )
        .children(output.map(|output| div().px(px(18.)).pb_3().child(output)))
        .into_any_element()
}

/// Hands a log file to the desktop's own opener.
fn open_button(id: impl Into<gpui::ElementId>, label: Cow<'static, str>, path: PathBuf) -> gpui::Stateful<Div> {
    button(id, ButtonKind::Secondary, label, false).on_click(move |_, _, _| {
        if let Err(err) = crate::desktop::open(&path) {
            tracing::warn!(path = %path.display(), %err, "could not open a Raven log");
        }
    })
}
