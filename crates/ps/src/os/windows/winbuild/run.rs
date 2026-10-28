//! Running a build's tool (`cmake.exe`, `ninja.exe`) confined: by absolute path, with no shell, in
//! a clean environment ([`os::build_env`]), inside a Job Object ([`os::confine`]), and
//! ended whole when it runs past its time or starts too many processes.

use std::ffi::OsString;
use std::path::Path;
use std::process::Stdio;
use std::time::{Duration, Instant};

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use tokio::io::{AsyncBufReadExt as _, AsyncRead, BufReader};
use tokio::process::Command;
use tokio::sync::mpsc;

use super::{Error, Log, MAX_LINE_BYTES};
use crate::os;

/// How often the job's processes are counted and its time checked.
const POLL: Duration = Duration::from_millis(500);

/// How long output is still read once the tool exited and its job was closed.
const DRAIN: Duration = Duration::from_secs(10);
/// Lines waiting for the log; past it, lines are dropped and counted rather than kept in memory.
const QUEUED_LINES: usize = 1024;

/// What one step may take.
#[derive(Debug, Clone, Copy)]
pub struct Bounds {
    pub timeout: Duration,
    /// Processes in the job at once: Ninja, its jobs and the `cmd.exe` wrapping CMake's custom
    /// commands, with room to spare; a count past it reads as a runaway.
    pub max_processes: usize,
}

/// One tool's run: `program`, an absolute path, with `args` in `dir`, seeing only `env`.
pub struct Step<'step> {
    /// What the log and errors call it.
    pub name: &'static str,
    pub program: &'step Path,
    pub args: &'step [OsString],
    pub dir: &'step Path,
    pub env: &'step [(&'static str, OsString)],
    pub bounds: Bounds,
}

/// `step`, its output to `log`, until it exits; then nothing it started is left running.
///
/// # Errors
/// When it can't start or be confined, exits with a failure, or passes its bounds.
pub async fn run(spec: Step<'_>, log: &mut Log<'_>) -> Result<(), Error> {
    let Step {
        name: step,
        program,
        args,
        dir,
        env,
        bounds,
    } = spec;
    let mut command = command(program, args, dir, env);
    log.line(format!("$ {} {}", program.display(), shown(args))).await;
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(err) => {
            return Err(Error::Tool {
                step,
                detail: err.to_string(),
            });
        }
    };
    let mut confined = match os::confine(&child) {
        Ok(confined) => Some(confined),
        Err(err) => {
            let _ = child.start_kill();
            return Err(Error::Confinement(err.to_string()));
        }
    };
    let Output {
        mut lines,
        readers,
        dropped,
    } = output(&mut child);
    let mut wait = std::pin::pin!(child.wait());
    let started = Instant::now();
    let mut poll = tokio::time::interval(POLL);
    let mut stopped: Option<Error> = None;
    let mut status = None;
    let mut exited_at: Option<Instant> = None;
    let mut open = true;
    while open || status.is_none() {
        tokio::select! {
            line = lines.recv(), if open => match line {
                Some(line) => log.line(line).await,
                None => open = false,
            },
            exit = &mut wait, if status.is_none() => {
                status = Some(exit.ok());
                exited_at = Some(Instant::now());
                // Whatever the tool left behind goes with it, and with them the pipes they held.
                if let Some(confined) = confined.take() {
                    confined.kill();
                }
            }
            _ = poll.tick() => {
                // A process that escaped the job may still hold the pipes; its output isn't waited for.
                if let Some(at) = exited_at {
                    open &= at.elapsed() <= DRAIN;
                    continue;
                }
                if stopped.is_some() {
                    continue;
                }
                let why = past_bounds(step, bounds, started, confined.as_ref());
                if let Some(why) = why {
                    log.line(format!("decanter: ending {step}: {why}")).await;
                    tracing::warn!(step, %why, "ending a Windows build step");
                    if let Some(confined) = confined.take() {
                        confined.kill();
                    }
                    stopped = Some(why);
                }
            }
        }
    }
    for reader in readers.into_iter().flatten() {
        reader.abort();
        let _ = reader.await;
    }
    let dropped = dropped.load(Ordering::Relaxed);
    if dropped > 0 {
        log.line(format!(
            "decanter: {dropped} lines of {step}'s output dropped while the log caught up"
        ))
        .await;
    }
    let status = status.flatten();
    if let Some(why) = stopped {
        return Err(why);
    }
    match status {
        Some(status) if status.success() => Ok(()),
        Some(status) => Err(Error::Tool {
            step,
            detail: status.to_string(),
        }),
        None => Err(Error::Tool {
            step,
            detail: "could not wait for it".to_owned(),
        }),
    }
}

/// A tool's output as it arrives: its lines, the tasks reading its pipes, and the lines dropped.
struct Output {
    lines: mpsc::Receiver<String>,
    readers: [Option<tokio::task::JoinHandle<()>>; 2],
    dropped: Arc<AtomicU64>,
}

fn output(child: &mut tokio::process::Child) -> Output {
    let (sender, lines) = mpsc::channel(QUEUED_LINES);
    let dropped = Arc::new(AtomicU64::new(0));
    let readers = [
        child
            .stdout
            .take()
            .map(|out| tokio::spawn(forward(out, sender.clone(), Arc::clone(&dropped)))),
        child
            .stderr
            .take()
            .map(|err| tokio::spawn(forward(err, sender.clone(), Arc::clone(&dropped)))),
    ];
    Output {
        lines,
        readers,
        dropped,
    }
}

/// `program` by absolute path, with nothing of decanter's environment but `env`, its output piped.
fn command(program: &Path, args: &[OsString], dir: &Path, env: &[(&'static str, OsString)]) -> Command {
    let mut command = Command::new(program);
    command
        .args(args)
        .current_dir(dir)
        .env_clear()
        .envs(env.iter().map(|(name, value)| (name, value)))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    os::own_process_group(&mut command);
    os::without_console(&mut command);
    command
}

/// Why the step must end now, if it ran past its time or holds too many processes.
fn past_bounds(step: &'static str, bounds: Bounds, started: Instant, confined: Option<&os::Confined>) -> Option<Error> {
    if started.elapsed() > bounds.timeout {
        return Some(Error::TimedOut {
            step,
            seconds: bounds.timeout.as_secs(),
        });
    }
    confined
        .map(os::Confined::processes)
        .filter(|&count| count > bounds.max_processes)
        .map(|count| Error::TooManyProcesses { step, count })
}

/// `stream` as lines of at most [`MAX_LINE_BYTES`] (the rest of a longer one is dropped, read but
/// never held), invalid UTF-8 replaced, sent while `sender` has room and counted in `dropped` when it
/// hasn't.
async fn forward(stream: impl AsyncRead + Unpin, sender: mpsc::Sender<String>, dropped: Arc<AtomicU64>) {
    let mut reader = BufReader::new(stream);
    let mut line = Vec::with_capacity(256);
    let mut cut = false;
    loop {
        let (taken, ended) = match reader.fill_buf().await {
            Ok([]) | Err(_) => break,
            Ok(buffer) => {
                let end = buffer.iter().position(|&byte| byte == b'\n');
                let chunk = buffer.get(..end.unwrap_or(buffer.len())).unwrap_or_default();
                let room = MAX_LINE_BYTES.saturating_sub(line.len());
                line.extend_from_slice(chunk.get(..chunk.len().min(room)).unwrap_or_default());
                cut |= chunk.len() > room;
                (end.map_or(buffer.len(), |end| end + 1), end.is_some())
            }
        };
        reader.consume(taken);
        if ended {
            send(&sender, &dropped, &line, cut);
            line.clear();
            cut = false;
        }
    }
    if !line.is_empty() {
        send(&sender, &dropped, &line, cut);
    }
}

fn send(sender: &mpsc::Sender<String>, dropped: &AtomicU64, line: &[u8], cut: bool) {
    let mut text = String::from_utf8_lossy(line).trim_end_matches('\r').to_owned();
    if cut {
        text.push_str(" [cut]");
    }
    if sender.try_send(text).is_err() {
        dropped.fetch_add(1, Ordering::Relaxed);
    }
}

/// The arguments as the log shows them, each quoted when it holds a space.
fn shown(args: &[OsString]) -> String {
    args.iter()
        .map(|arg| {
            let arg = arg.to_string_lossy();
            if arg.contains(' ') {
                format!("\"{arg}\"")
            } else {
                arg.into_owned()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}
