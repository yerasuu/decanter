//! Steam entries, started by Steam itself through
//! [`steam://rungameid/<appid>`](https://developer.valvesoftware.com/wiki/Steam_browser_protocol),
//! the command Steam's own desktop entry hands URLs to (`steam %U`).
//!
//! The game is Steam's child, not decanter's, so its session is followed
//! from `/proc`: Steam sets `SteamAppId` to the app id on the actual game
//! and everything it starts, and not on install scripts or other setup
//! commands ([Steam compat tool interface: environment
//! variables](https://gitlab.steamos.cloud/steamrt/steam-runtime-tools/-/blob/main/docs/steam-compat-tool-interface.md#environment)).
//! The session runs while any process of this user carries it. Steam
//! documents no command to stop a running game, so Stop asks the game's
//! processes to end, as decanter's own sessions are asked.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use decanter_core::InstalledGame;
use decanter_ps::process::{Event, Job, JobKind, State};
use gpui::{AppContext as _, AsyncApp, Entity};
use rust_i18n::t;

use crate::external::steam::AppId;
use crate::external::steam::entries;
use crate::state::Background;

/// How long the game gets to appear after Steam was asked to start it:
/// long enough for Steam to start itself and for a short update or
/// first-run setup before the game, not for a long download.
const START_TIMEOUT: Duration = Duration::from_secs(300);

const POLL: Duration = Duration::from_secs(1);

/// Polls in a row without a process before the session counts as over, so
/// that a launcher handing over to the game proper is not taken for an end.
const GONE_POLLS: u32 = 3;

/// How long a stopped game gets to close on `SIGTERM` before `SIGKILL`.
const STOP_GRACE: Duration = Duration::from_secs(10);

/// Whether `game_id` of `account_id` is a Steam entry, which Steam starts.
pub(super) fn entry(account_id: &str, game_id: &str) -> Option<AppId> {
    crate::core::installed_game(account_id, game_id).and_then(|game| entries::app_id(&game))
}

/// Asks Steam to start `game` and follows the session it starts; how long
/// the game ran, or `None` if it never showed up.
pub(super) async fn start(
    account_id: &str,
    game: &InstalledGame,
    background: &Entity<Background>,
    cx: &mut AsyncApp,
) -> Option<Duration> {
    let Some(app_id) = entries::app_id(game) else {
        tracing::error!(game_id = %game.game_id, "steam entry has no valid app id");
        return None;
    };
    let game_id = game.game_id.as_str();
    let job = cx.new(|_| Job {
        kind: JobKind::Game,
        state: State::Running(0),
        lines: VecDeque::new(),
        stop_requested: false,
        log_file: None,
    });
    background.update(cx, |background, cx| {
        background.runs.insert(game_id.to_owned(), job.clone());
        cx.notify();
    });
    if find(app_id).await.is_empty() {
        if let Err(err) = open(app_id) {
            tracing::error!(game_id, app_id = %app_id, %err, "could not hand the game to steam");
            end_job(
                &job,
                None,
                t!("library.steam.log.open_failed", error = err).into_owned(),
                cx,
            );
            return None;
        }
        tracing::info!(game_id, app_id = %app_id, "game handed to steam");
        push(&job, t!("library.steam.log.requested").into_owned(), cx);
    } else {
        tracing::info!(game_id, app_id = %app_id, "game already running under steam, following it");
    }
    if !processes::CAN_FOLLOW {
        tracing::warn!(game_id, app_id = %app_id, "cannot follow a steam game on this system, no play time recorded");
        end_job(&job, None, t!("library.steam.log.unfollowed").into_owned(), cx);
        return None;
    }

    set_state(background, game_id, Phase::Starting, cx);
    let Some(pid) = wait_for_game(app_id, &job, cx).await else {
        set_state(background, game_id, Phase::Ended, cx);
        give_up(game_id, app_id, &job, cx);
        return None;
    };
    tracing::info!(game_id, app_id = %app_id, pid, "steam game running");
    job.update(cx, |job, cx| {
        job.state = State::Running(pid);
        job.push(Event::Line(t!("library.steam.log.running", pid = pid).into_owned()));
        cx.notify();
    });
    set_state(background, game_id, Phase::Running, cx);
    let target = crate::closing::Target::Pc {
        account_id: account_id.to_owned(),
        game_id: game.game_id.clone(),
    };
    cx.update(|cx| crate::closing::started(target.clone(), game.name.clone(), cx));
    let started = Instant::now();
    let mut gone = 0;
    while gone < GONE_POLLS {
        tokio::time::sleep(POLL).await;
        gone = if find(app_id).await.is_empty() { gone + 1 } else { 0 };
    }
    let elapsed = started.elapsed().saturating_sub(POLL * GONE_POLLS);
    cx.update(|cx| crate::closing::ended(&target, cx));
    // The game is Steam's child, so its exit status never reaches decanter;
    // a session that ended by itself reads as finished, not interrupted.
    let code = (!job.read_with(cx, |job, _| job.stop_requested)).then_some(0);
    end_job(&job, code, t!("library.steam.log.ended").into_owned(), cx);
    set_state(background, game_id, Phase::Ended, cx);
    tracing::info!(game_id, app_id = %app_id, duration_secs = elapsed.as_secs(), "steam game session ended");
    super::record_last_played(account_id, game);
    Some(elapsed)
}

/// Ends the game's processes: `SIGTERM` to each, again every tick for any
/// that appear meanwhile, then `SIGKILL` to what outlived [`STOP_GRACE`].
pub(super) async fn stop(game_id: &str, app_id: AppId) {
    let asked = Instant::now();
    loop {
        let pids = find(app_id).await;
        if pids.is_empty() {
            tracing::info!(game_id, app_id = %app_id, "steam game stopped");
            return;
        }
        let force = asked.elapsed() >= STOP_GRACE;
        if force {
            tracing::warn!(game_id, app_id = %app_id, count = pids.len(), "steam game outlived the grace, killing it");
        }
        for pid in pids {
            processes::signal(pid, force);
        }
        tokio::time::sleep(POLL).await;
    }
}

pub(super) fn is_running(app_id: AppId) -> bool {
    !processes::of(app_id).is_empty()
}

/// [`processes::of`] off the UI thread: it reads every process's
/// environment, once a second for as long as a game runs.
async fn find(app_id: AppId) -> Vec<u32> {
    tokio::task::spawn_blocking(move || processes::of(app_id))
        .await
        .inspect_err(|err| tracing::warn!(app_id = %app_id, %err, "steam game process scan did not finish"))
        .unwrap_or_default()
}

/// Waits for the first process of the game, while nobody stopped the wait
/// and [`START_TIMEOUT`] has not passed.
async fn wait_for_game(app_id: AppId, job: &Entity<Job>, cx: &AsyncApp) -> Option<u32> {
    let asked = Instant::now();
    loop {
        if let Some(pid) = find(app_id).await.into_iter().min() {
            return Some(pid);
        }
        if asked.elapsed() >= START_TIMEOUT || job.read_with(cx, |job, _| job.stop_requested) {
            return None;
        }
        tokio::time::sleep(POLL).await;
    }
}

/// Closes the run of a game that never showed up: a Stop pressed while
/// waiting, or [`START_TIMEOUT`].
fn give_up(game_id: &str, app_id: AppId, job: &Entity<Job>, cx: &mut AsyncApp) {
    if job.read_with(cx, |job, _| job.stop_requested) {
        tracing::info!(game_id, app_id = %app_id, "stopped waiting for the steam game");
        end_job(job, None, t!("library.steam.log.cancelled").into_owned(), cx);
    } else {
        tracing::warn!(game_id, app_id = %app_id, timeout_secs = START_TIMEOUT.as_secs(), "steam game did not start in time");
        end_job(job, None, t!("library.steam.log.timeout").into_owned(), cx);
    }
}

/// `steam steam://rungameid/<appid>`, the way Steam's desktop entry runs a
/// URL, started as every other start of Steam's is
/// ([`decanter_core::steam::process::open_url`]) so that a Steam this
/// starts is never in decanter's cgroup; else the desktop's opener, which
/// finds Steam as the `steam` scheme's handler. Never through a shell.
fn open(app_id: AppId) -> std::io::Result<()> {
    // Built from a checked id, so it never starts with `-` and no argument
    // terminator is needed (Steam documents none).
    let url = format!("steam://rungameid/{app_id}");
    let Some(root) = decanter_core::steam::steam_root() else {
        tracing::debug!(app_id = %app_id, "no steam root, handing the url to the desktop");
        return crate::desktop::open(&url);
    };
    decanter_core::steam::process::open_url(&root, &url).or_else(|err| {
        tracing::debug!(app_id = %app_id, %err, "could not run steam, handing the url to the desktop");
        crate::desktop::open(&url)
    })
}

#[derive(Clone, Copy)]
enum Phase {
    Starting,
    Running,
    Ended,
}

/// `running` holds the game from the moment Steam is asked, so that Stop
/// is there to give up a wait; `starting` says the game has not shown up.
fn set_state(background: &Entity<Background>, game_id: &str, phase: Phase, cx: &mut AsyncApp) {
    background.update(cx, |background, cx| {
        match phase {
            Phase::Starting => {
                background.running.insert(game_id.to_owned());
                background.starting.insert(game_id.to_owned());
            }
            Phase::Running => {
                background.starting.remove(game_id);
            }
            Phase::Ended => {
                background.running.remove(game_id);
                background.starting.remove(game_id);
            }
        }
        cx.notify();
    });
}

fn push(job: &Entity<Job>, line: String, cx: &mut AsyncApp) {
    job.update(cx, |job, cx| {
        job.push(Event::Line(line));
        cx.notify();
    });
}

fn end_job(job: &Entity<Job>, code: Option<i32>, line: String, cx: &mut AsyncApp) {
    job.update(cx, |job, cx| {
        job.push(Event::Line(line));
        job.state = State::Exited(code);
        cx.notify();
    });
}

/// The game's processes: this user's, with `SteamAppId=<appid>` in their
/// environment. Read defensively, since any of them may end mid-read and
/// other users' are not readable at all.
#[cfg(target_os = "linux")]
mod processes {
    use std::os::unix::fs::MetadataExt as _;

    use nix::sys::signal::{Signal, kill};
    use nix::unistd::{Pid, Uid};

    use super::AppId;

    pub const CAN_FOLLOW: bool = true;

    pub fn of(app_id: AppId) -> Vec<u32> {
        let Ok(entries) = std::fs::read_dir("/proc") else {
            tracing::warn!("/proc unreadable, no steam game process can be found");
            return Vec::new();
        };
        let wanted = format!("SteamAppId={app_id}");
        let uid = Uid::current().as_raw();
        let own = std::process::id();
        entries
            .flatten()
            .filter_map(|entry| {
                let pid = entry
                    .file_name()
                    .to_str()?
                    .parse::<u32>()
                    .ok()
                    .filter(|pid| *pid != own)?;
                let path = entry.path();
                if path.metadata().ok()?.uid() != uid {
                    return None;
                }
                let environ = std::fs::read(path.join("environ")).ok()?;
                environ
                    .split(|byte| *byte == 0)
                    .any(|variable| variable == wanted.as_bytes())
                    .then_some(pid)
            })
            .collect()
    }

    pub fn signal(pid: u32, force: bool) {
        let Ok(raw) = i32::try_from(pid) else {
            tracing::warn!(pid, "pid out of range for a signal, skipping");
            return;
        };
        let signal = if force { Signal::SIGKILL } else { Signal::SIGTERM };
        // Racy by nature: the process may be gone already, which is what
        // was asked for.
        let result = kill(Pid::from_raw(raw), signal);
        tracing::trace!(pid, ?signal, ?result, "signalled steam game process");
    }
}

/// Elsewhere no process's environment is read, so Steam starts the game and
/// nothing follows it.
#[cfg(not(target_os = "linux"))]
mod processes {
    use super::AppId;

    pub const CAN_FOLLOW: bool = false;

    pub const fn of(_: AppId) -> Vec<u32> {
        Vec::new()
    }

    pub const fn signal(_: u32, _: bool) {}
}
