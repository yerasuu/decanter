//! Running a port's process to completion with its output in an entity of
//! its own, so a chatty game redraws only the log that shows it.

use std::path::PathBuf;

use decanter_ps::failures::{Failure, Scanner};
use decanter_ps::port::Port;
use decanter_ps::process::{self, Event, Job};
use gpui::{AppContext as _, AsyncApp, Entity, WeakEntity};
use tokio::process::Command;

use super::Ps;

/// Lines taken per redraw: a burst of output costs one notify, not one per line.
const BATCH: usize = 256;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum JobKey {
    /// Cloning, building or updating a port.
    Port(Port),
    /// Playing or converting the game in this folder.
    Game(PathBuf),
}

pub struct Finished {
    /// `None` when it never started or was killed by a signal.
    pub code: Option<i32>,
    /// What the scanner found, when the job had one.
    pub failures: Vec<Failure>,
}

impl Finished {
    pub const fn succeeded(&self) -> bool {
        matches!(self.code, Some(0))
    }
}

/// Replaces whatever job `key` held before; its log goes with it.
pub async fn run(
    ps: &WeakEntity<Ps>,
    key: JobKey,
    label: &str,
    command: Command,
    mut scanner: Option<Scanner>,
    cx: &mut AsyncApp,
) -> Finished {
    let (job, mut events) = match process::spawn(label, command) {
        Ok(spawned) => spawned,
        Err(err) => {
            tracing::warn!(label, %err, "could not start job");
            let job = cx.new(|_| Job::failed(label, format!("No se pudo iniciar: {err}")));
            let _ = ps.update(cx, |ps, cx| {
                ps.jobs.insert(key, job);
                cx.notify();
            });
            return Finished {
                code: None,
                failures: Vec::new(),
            };
        }
    };
    let job: Entity<Job> = cx.new(|_| job);
    if ps
        .update(cx, |ps, cx| {
            ps.jobs.insert(key, job.clone());
            cx.notify();
        })
        .is_err()
    {
        return Finished {
            code: None,
            failures: Vec::new(),
        };
    }
    let mut batch = Vec::with_capacity(BATCH);
    let mut code = None;
    while events.recv_many(&mut batch, BATCH).await > 0 {
        job.update(cx, |job, cx| {
            for event in batch.drain(..) {
                match (&event, scanner.as_mut()) {
                    (Event::Line(line), Some(scanner)) => scanner.line(line),
                    (Event::Exited(exit), _) => code = *exit,
                    (Event::Line(_), None) => {}
                }
                job.push(event);
            }
            cx.notify();
        });
    }
    let stopped = job.read_with(cx, |job, _| job.stop_requested);
    Finished {
        code,
        failures: scanner.map(|scanner| scanner.exit(code, stopped)).unwrap_or_default(),
    }
}
