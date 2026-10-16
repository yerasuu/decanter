//! Starting and stopping a game, shared by the library's PLAY button, the
//! right-click menu, and the compatibility screen.
//!
//! The running-state bookkeeping lives here once instead of at each of
//! those call sites. Every caller runs [`start`] as a detached app-level
//! task, not a view's: it writes to `running`, which swaps the button that
//! started it out of existence.

use decanter_core::InstalledGame;
use decanter_ps::process::{self, Event, Job, JobKind};
use decanter_ps::run_log::{RunLog, describe_exit};
use decanter_runtime::Launch;
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
/// prefix has anything live in it.
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
    let Some(prefix) = crate::core::prefix_for(&account_id, &game.game_id) else {
        tracing::error!(game_id = %game.game_id, "no prefix path");
        return None;
    };

    let cloud_save_checkpoint = sync_cloud_saves_before_launch(&account_id, &game).await;

    // `umu-<product id>` with `STORE=gog` is how protonfixes finds a GOG
    // game's workarounds. An external game's uuid is in no database, so it
    // goes in as `umu-default` with no store instead — the same as Heroic
    // runs a sideloaded game — which some Proton builds (DW-Proton) map to
    // the right id from the executable; a made-up id skipped that, and with
    // it fixes a game like Arknights: Endfield can't start without.
    // One picked from the UMU database by name goes in as that entry, store
    // and all, which works with any Proton carrying protonfixes.
    let mut launch = match (game.storefront, &game.umu_id) {
        (decanter_core::Storefront::Gog, _) => Launch::new(executable, format!("umu-{}", game.game_id)).store("gog"),
        (_, Some(id)) => {
            let launch = Launch::new(executable, id.as_str());
            match &game.umu_store {
                Some(store) => launch.store(store.as_str()),
                None => launch,
            }
        }
        (_, None) => Launch::new(executable, crate::external::UMU_ID),
    };
    launch.proton = crate::core::proton_for(&game);
    let key = crate::taskbar::key(game.storefront, &game.game_id);
    let mangohud = crate::core::db()
        .ok()
        .and_then(|db| crate::game_tools::mangohud(&db, &key));
    if let Some(autostart_log) = mangohud {
        match crate::core::library_for_account(&account_id) {
            Ok(library) => launch.env.extend(
                crate::game_tools::mangohud_env(&key, library.root(), autostart_log)
                    .await
                    .map(|(name, value)| (name.to_owned(), value)),
            ),
            Err(err) => {
                tracing::warn!(game_id = %game.game_id, %err, "no library to log MangoHud to, starting without it")
            }
        }
    }
    launch.identity = Some((key, game.name.clone()));

    // decanter-hook (a GOG-only concern -- a shortcut has no Galaxy DLL
    // to patch) reads both of these itself; setting them here is what
    // lets it write somewhere decanter actually knows to look afterward,
    // and tag what it wrote with the right product id.
    let achievement_log = (game.storefront == decanter_core::Storefront::Gog).then(|| {
        let path = decanter_gog::hook::achievement_log_path(&prefix);
        launch
            .env
            .push(("DECANTER_PRODUCT_ID".to_owned(), game.game_id.clone()));
        launch
            .env
            .push(("DECANTER_ACHIEVEMENT_LOG".to_owned(), path.display().to_string()));
        path
    });

    let Some((job, events, log)) = spawn(&account_id, &game, &prefix, launch, &background, cx).await else {
        return None;
    };

    set_running(&background, &game.game_id, true, cx);
    let target = crate::closing::Target::Pc {
        account_id: account_id.clone(),
        game_id: game.game_id.clone(),
    };
    cx.update(|cx| crate::closing::started(target.clone(), game.name.clone(), cx));
    let started = std::time::Instant::now();
    let exit = crate::run_log::follow(&job, events, log, &game.game_id, cx).await;
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
    prefix: &decanter_runtime::Prefix,
    mut launch: Launch,
    background: &Entity<Background>,
    cx: &mut AsyncApp,
) -> Option<(Entity<Job>, UnboundedReceiver<Event>, Option<RunLog>)> {
    launch.capture_output = true;
    let mut prepared = match decanter_runtime::prepare(prefix, launch).await {
        Ok(prepared) => prepared,
        Err(err) => {
            tracing::error!(game_id = %game.game_id, error = %format_args!("{err:#}"), "launch failed");
            return None;
        }
    };
    // A run goes on without its file rather than not at all; the log view says so.
    let (log, log_error) = match crate::run_log::start(account_id, game, prefix.path(), &mut prepared).await {
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
                crate::run_log::abandon(log, &game.game_id, &reason).await;
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
    let Some(path) = achievement_log else {
        return;
    };
    let Ok(mut db) = decanter_core::Db::open_active() else {
        tracing::debug!(game_id = %game.game_id, "could not open db, decanter-hook log not applied");
        return;
    };
    if let Err(err) = decanter_gog::poll_achievements(&mut db, account_id, &game.game_id).await {
        tracing::warn!(game_id = %game.game_id, %err, "post-session achievement poll failed");
    }
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
///
/// Prefix-wide, which is now the same thing as game-wide: each game has
/// its own prefix, so stopping one can't reach another.
pub async fn stop(account_id: String, game_id: String, background: Entity<Background>, cx: &mut AsyncApp) {
    // So the exit this causes reads as a stop in the run's log, not as a crash.
    background.update(cx, |background, cx| {
        if let Some(job) = background.runs.get(&game_id) {
            job.update(cx, |job, _| job.stop_requested = true);
        }
    });
    let Some(prefix) = crate::core::prefix_for(&account_id, &game_id) else {
        tracing::warn!(account_id, game_id, "cannot stop, no prefix resolved");
        return;
    };
    match prefix.stop().await {
        Ok(()) => tracing::info!(account_id, game_id, "game stopped"),
        Err(err) => tracing::error!(account_id, game_id, error = %format_args!("{err:#}"), "stop failed"),
    }
}

/// Stops every game decanter started.
///
/// Games outlive the app that launched them — umu execs into a container
/// and Proton detaches, so nothing here is a child process by the time it
/// matters. Closing decanter without this leaves them running with no way
/// left to reach them from the UI.
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

/// Throws a game's prefix away so umu builds a fresh one next launch.
///
/// The escape hatch for a prefix a game has wrecked — the reason each
/// game gets its own. Saves a game keeps inside its prefix go with it,
/// which is why this is never automatic.
pub async fn reset_prefix(account_id: String, game_id: String) -> Result<(), String> {
    let Some(prefix) = crate::core::prefix_for(&account_id, &game_id) else {
        return Err(t!("messages.errors.no_prefix").into_owned());
    };
    // Nothing may be running in it: deleting a live prefix out from under
    // Wine corrupts whatever it was writing.
    crate::achv_try!(
        prefix
            .stop()
            .await
            .inspect_err(
                |err| tracing::error!(account_id, game_id, %err, "failed to stop game before resetting its prefix")
            )
            .map_err(|err| err.to_string())
    );
    match tokio::fs::remove_dir_all(prefix.path()).await {
        Ok(()) => {
            tracing::info!(account_id, game_id, "prefix reset");
            Ok(())
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            tracing::debug!(account_id, game_id, "prefix already absent, nothing to reset");
            Ok(())
        }
        Err(err) => {
            tracing::error!(account_id, game_id, %err, "failed to remove prefix directory");
            Err(t!("messages.errors.delete_prefix", error = err).into_owned())
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
