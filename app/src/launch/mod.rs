//! Starting and stopping a game, shared by the library's PLAY button, the
//! right-click menu, and the compatibility screen.
//!
//! The running-state bookkeeping lives here once instead of at each of
//! those call sites. Every caller runs [`start`] as a detached app-level
//! task, not a view's: it writes to `running`, which swaps the button that
//! started it out of existence.
//!
//! How a game is started and stopped is the backend's: Proton in a prefix
//! of its own (`proton`), or the executable itself on Windows (`native`).

#[cfg(windows)]
mod native;
#[cfg(windows)]
mod pe;
#[cfg(not(windows))]
mod proton;

#[cfg(windows)]
use native as backend;
#[cfg(windows)]
pub use native::is_running;
#[cfg(not(windows))]
use proton as backend;
#[cfg(not(windows))]
pub use proton::reset_prefix;

use decanter_core::InstalledGame;
use decanter_ps::process::{self, Event, Job, JobKind};
use decanter_ps::run_log::{RunLog, describe_exit};
use gpui::{AppContext as _, AsyncApp, Entity};
use rust_i18n::t;
use tokio::sync::mpsc::UnboundedReceiver;

use crate::state::Background;

fn set_running(background: &Entity<Background>, game_id: &str, running: bool, cx: &mut AsyncApp) {
    background.update(cx, |background, cx| {
        if running {
            background.running.insert(game_id.to_owned());
        } else {
            background.running.remove(game_id);
        }
        cx.notify();
    });
}

/// Runs `game` to completion, holding it in `running` for as long as its
/// session lasts.
///
/// Returns how long the session lasted, or `None` if it never started —
/// the caller decides whether that's worth recording as playtime.
pub async fn start(
    account_id: String,
    game: InstalledGame,
    background: Entity<Background>,
    cx: &mut AsyncApp,
) -> Option<std::time::Duration> {
    let Some(executable) = crate::core::executable_for(&game) else {
        tracing::error!(game_id = %game.game_id, "no launch executable");
        return None;
    };
    if !executable.is_file() {
        tracing::error!(game_id = %game.game_id, path = %executable.display(), "launch executable is not on disk");
        return None;
    }
    #[cfg(windows)]
    let hinted = executable.clone();
    let Some(mut planned) = backend::plan(&account_id, &game, executable) else {
        return None;
    };

    let cloud_save_checkpoint = sync_cloud_saves_before_launch(&account_id, &game).await;
    let achievement_log = planned.achievement_log.take();

    let Some((job, events, log)) = spawn(&account_id, &game, planned, &background, cx).await else {
        return None;
    };

    set_running(&background, &game.game_id, true, cx);
    let target = crate::closing::Target::Pc {
        account_id: account_id.clone(),
        game_id: game.game_id.clone(),
    };
    cx.update(|cx| crate::closing::started(target.clone(), game.name.clone(), cx));
    let started = std::time::Instant::now();
    let (exit, log) = crate::run_log::follow(&job, events, log, &game.game_id, cx).await;
    #[cfg(windows)]
    let log = backend::hint_after_failure(&hinted, exit, &job, log, &game.game_id, cx).await;
    crate::run_log::finish(log, exit, &game.game_id).await;
    cx.update(|cx| crate::closing::ended(&target, cx));
    set_running(&background, &game.game_id, false, cx);
    let elapsed = started.elapsed();
    tracing::info!(
        game_id = %game.game_id,
        exit = describe_exit(exit),
        duration_secs = elapsed.as_secs(),
        "game session ended"
    );

    sync_cloud_saves_after_exit(&account_id, &game, cloud_save_checkpoint).await;
    record_last_played(&account_id, &game);
    apply_hook_log(&account_id, &game, achievement_log.as_deref()).await;
    Some(elapsed)
}

/// Starts `launch` with its output followed into the game's run, which the
/// library's Registro tab shows; `None` when it never started, with the
/// reason in that run.
async fn spawn(
    account_id: &str,
    game: &InstalledGame,
    mut planned: backend::Planned,
    background: &Entity<Background>,
    cx: &mut AsyncApp,
) -> Option<(Entity<Job>, UnboundedReceiver<Event>, Option<RunLog>)> {
    let facts = core::mem::take(&mut planned.facts);
    let mut prepared = match planned.prepare().await {
        Ok(prepared) => prepared,
        Err(err) => {
            tracing::error!(game_id = %game.game_id, error = %format_args!("{err:#}"), "launch failed");
            return None;
        }
    };
    // A run goes on without its file rather than not at all; the log view says so.
    let (log, log_error) = match crate::run_log::start(account_id, &game.game_id, &facts, &mut prepared).await {
        Ok(log) => (Some(log), None),
        Err(err) => {
            tracing::warn!(game_id = %game.game_id, %err, "could not create the run's log file");
            (None, Some(t!("ps.jobs.log_error", error = err).into_owned()))
        }
    };

    // Before the window can appear, so the task bar already knows it.
    crate::taskbar::sync_pc(game).await;
    let mut session = match prepared.spawn() {
        Ok(session) => session,
        Err(err) => {
            tracing::error!(game_id = %game.game_id, error = %format_args!("{err:#}"), "launch failed");
            let reason = format!("{err:#}");
            if let Some(log) = log {
                crate::run_log::abandon(log, &game.game_id, backend::LAUNCHER, &reason).await;
            }
            let job = cx.new(|_| Job::failed(JobKind::Game, t!("ps.jobs.start_error", error = reason).into_owned()));
            background.update(cx, |background, cx| {
                background.runs.insert(game.game_id.clone(), job);
                cx.notify();
            });
            return None;
        }
    };
    tracing::info!(
        game_id = %game.game_id,
        name = %game.name,
        log = log.as_ref().map(|log| tracing::field::display(log.path().display())),
        "game launched"
    );
    let (stdout, stderr) = session.take_output();
    let waited = game.game_id.clone();
    let (mut job, events) = process::follow(JobKind::Game, session.launcher_pid(), stdout, stderr, async move {
        match session.wait_status().await {
            Ok(status) => Some(status),
            Err(err) => {
                tracing::warn!(game_id = %waited, error = %format_args!("{err:#}"), "game session ended with an error");
                None
            }
        }
    });
    job.log_file = log.as_ref().map(|log| log.path().to_owned());
    if let Some(error) = log_error {
        job.push(Event::Line(error));
    }
    let job = cx.new(|_| job);
    background.update(cx, |background, cx| {
        background.runs.insert(game.game_id.clone(), job.clone());
        cx.notify();
    });

    Some((job, events, log))
}

/// Folds in whatever decanter-hook recorded this session: a locally
/// witnessed achievement unlock GOG's own servers never hear about under
/// Wine, and any failure to install its vtable patch in the first place.
/// Without a hook log (a game run natively) only the poll runs, which is
/// how decanter hears of what the game's own Galaxy integration unlocked.
/// Runs after [`record_last_played`], not before: neither ordering
/// matters to the other, so this just goes last as the newest addition.
///
/// Polls GOG first, same as decanter's own CLI launch path already does
/// after every session -- [`decanter_gog::hook::process_log`] can only
/// attach a local unlock to an achievement this game's cache already
/// knows the name of, so a game whose achievements were never fetched
/// yet (never opened past its library card) would otherwise lose that
/// unlock the moment the log is removed, with no cache row to retry it
/// against later.
async fn apply_hook_log(account_id: &str, game: &InstalledGame, achievement_log: Option<&std::path::Path>) {
    if game.storefront != decanter_core::Storefront::Gog {
        return;
    }
    let Ok(mut db) = decanter_core::Db::open_active() else {
        tracing::debug!(game_id = %game.game_id, "could not open db, decanter-hook log not applied");
        return;
    };
    if let Err(err) = decanter_gog::poll_achievements(&mut db, account_id, &game.game_id).await {
        tracing::warn!(game_id = %game.game_id, %err, "post-session achievement poll failed");
    }
    let Some(path) = achievement_log else {
        return;
    };
    let applied = decanter_gog::hook::process_log(&db, account_id, &game.game_id, path).await;
    if applied > 0 {
        tracing::info!(game_id = %game.game_id, applied, "applied locally-witnessed achievement unlocks");
    }
}

/// Pulls cloud saves down before a session starts, if this game has a
/// folder configured — best-effort, same as [`record_last_played`]: a
/// failure here costs a stale save, not the launch itself.
///
/// Returns the sync checkpoint to classify *this session's* changes
/// against at exit — `game.cloud_save_last_sync` is what it was before
/// this ran, and a successful sync here already moved it forward, so
/// reusing the stale value at exit would tell the classifier a session's
/// worth of files "changed since" a timestamp cloud saves already knows
/// about, and re-push them for nothing.
async fn sync_cloud_saves_before_launch(account_id: &str, game: &InstalledGame) -> Option<i64> {
    let Some(local_root) = game.cloud_save_path.as_deref() else {
        return game.cloud_save_last_sync;
    };
    match decanter_gog::cloud_saves::sync(
        account_id,
        &game.game_id,
        std::path::Path::new(local_root),
        game.cloud_save_last_sync,
        None,
    )
    .await
    {
        Ok(outcome) if outcome.action == decanter_gog::cloud_saves::SyncAction::Conflict => {
            tracing::warn!(
                game_id = %game.game_id,
                "cloud saves conflict before launch, playing with the local copy until resolved"
            );
            game.cloud_save_last_sync
        }
        Ok(outcome) => {
            if let Err(err) = crate::core::record_cloud_save_sync(account_id, &game.game_id, outcome.last_sync) {
                tracing::warn!(game_id = %game.game_id, error = %err, "could not record the cloud saves sync");
            }
            tracing::info!(game_id = %game.game_id, action = ?outcome.action, "cloud saves synced before launch");
            Some(outcome.last_sync)
        }
        Err(err) => {
            tracing::warn!(game_id = %game.game_id, error = %format_args!("{err:#}"), "cloud saves sync before launch failed");
            game.cloud_save_last_sync
        }
    }
}

/// Pushes local changes up once a session ends, mirroring
/// [`sync_cloud_saves_before_launch`]. `checkpoint` is that call's return
/// value, not `game.cloud_save_last_sync` — see its doc for why.
async fn sync_cloud_saves_after_exit(account_id: &str, game: &InstalledGame, checkpoint: Option<i64>) {
    let Some(local_root) = game.cloud_save_path.as_deref() else {
        return;
    };
    match decanter_gog::cloud_saves::sync(
        account_id,
        &game.game_id,
        std::path::Path::new(local_root),
        checkpoint,
        None,
    )
    .await
    {
        Ok(outcome) if outcome.action == decanter_gog::cloud_saves::SyncAction::Conflict => {
            tracing::warn!(game_id = %game.game_id, "cloud saves conflict after exit, left unresolved");
        }
        Ok(outcome) => {
            if let Err(err) = crate::core::record_cloud_save_sync(account_id, &game.game_id, outcome.last_sync) {
                tracing::warn!(game_id = %game.game_id, error = %err, "could not record the cloud saves sync");
            }
            tracing::info!(game_id = %game.game_id, action = ?outcome.action, "cloud saves synced after exit");
        }
        Err(err) => {
            tracing::warn!(game_id = %game.game_id, error = %format_args!("{err:#}"), "cloud saves sync after exit failed");
        }
    }
}

/// Ends a game's session.
pub async fn stop(account_id: String, game_id: String, background: Entity<Background>, cx: &mut AsyncApp) {
    // So the exit this causes reads as a stop in the run's log, not as a crash.
    background.update(cx, |background, cx| {
        if let Some(job) = background.runs.get(&game_id) {
            job.update(cx, |job, _| job.stop_requested = true);
        }
    });
    backend::stop(&account_id, &game_id).await;
}

/// Stops every game decanter started.
///
/// Games outlive the app that launched them — umu execs into a container
/// and Proton detaches, and a native game is its own process — so closing
/// decanter without this leaves them running with no way left to reach
/// them from the UI.
pub async fn stop_everything() {
    tracing::info!("stopping all running games");
    decanter_runtime::stop_all().await;
}

/// Stops everything and exits, for the signals a desktop actually sends —
/// a session logging out, `pkill`, Ctrl+C from a terminal.
///
/// Only the polite ones: `SIGKILL` can't be caught, so a hard kill still
/// leaves games running. Nothing can change that.
pub async fn exit_with_games_on_signal(cx: &mut AsyncApp) {
    if !exit_signal().await {
        return;
    }
    stop_everything().await;

    // Not `std::process::exit` -- that tears the process down while other
    // tokio worker threads (a still-in-flight GOG library fetch, say) may
    // still be inside OpenSSL, racing its own `atexit` cleanup and
    // segfaulting in `OPENSSL_cleanup` (confirmed via coredumpctl: SIGSEGV
    // in `ossl_lib_ctx_default_deinit`, concurrent with a `spawn_blocking`
    // GOG session call). Quitting GPUI instead returns from `main`'s run
    // loop, and the tokio runtime `main` owns is then dropped the ordinary
    // way -- which blocks until outstanding `spawn_blocking` work finishes,
    // so nothing is still touching OpenSSL by the time the process exits.
    cx.update(|cx| cx.quit());
}

/// Resolves once one of those signals came, or `false` at once when they
/// can't be watched.
#[cfg(unix)]
async fn exit_signal() -> bool {
    let mut term = match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
        Ok(signal) => signal,
        Err(err) => {
            tracing::warn!(signal = "SIGTERM", %err, "cannot watch for the signal");
            return false;
        }
    };
    let mut interrupt = match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt()) {
        Ok(signal) => signal,
        Err(err) => {
            tracing::warn!(signal = "SIGINT", %err, "cannot watch for the signal");
            return false;
        }
    };

    tokio::select! {
        _ = term.recv() => tracing::info!("received sigterm, stopping games before exit"),
        _ = interrupt.recv() => tracing::info!("received sigint, stopping games before exit"),
    }
    true
}

/// Only Ctrl+C has a signal here; logging off closes the window, which
/// quits the ordinary way.
#[cfg(not(unix))]
async fn exit_signal() -> bool {
    match tokio::signal::ctrl_c().await {
        Ok(()) => {
            tracing::info!("received ctrl-c, stopping games before exit");
            true
        }
        Err(err) => {
            tracing::warn!(signal = "ctrl-c", %err, "cannot watch for the signal");
            false
        }
    }
}

/// Stamps the manifest with when the session ended. Best-effort: a failed
/// write costs a sort order, not a game.
fn record_last_played(account_id: &str, game: &InstalledGame) {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_secs() as i64);
    let Ok(library) = crate::core::library_for_account(account_id) else {
        tracing::debug!(game_id = %game.game_id, "could not record last played, library unavailable");
        return;
    };
    // Re-read rather than reusing the copy this launch started from: the
    // session may have outlasted an edit to the manifest.
    let mut current = library
        .read_manifest(account_id, game.storefront, &game.game_id)
        .unwrap_or_else(|| game.clone());
    current.last_played = Some(now);
    if let Err(err) = library.write_manifest(&current) {
        tracing::warn!(game_id = %game.game_id, %err, "could not record last played");
    }
}
