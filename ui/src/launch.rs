//! Starting and stopping a game, shared by the library's PLAY button, the
//! right-click menu, and the compatibility screen.
//!
//! The running-state bookkeeping lives here once instead of at each of
//! those call sites. Every caller uses `spawn_forever`, not `spawn`:
//! [`start`] writes to `running`, which swaps the button that started it
//! out of existence, and a plain `spawn` would be cancelled by that very
//! re-render.

use std::collections::HashSet;

use achv_core::InstalledGame;
use achv_runtime::Launch;
use dioxus::prelude::*;

/// Games in play, by game id. Not by prefix: a prefix belongs to exactly
/// one (profile, game) now, so the game id is the finer key and the one
/// every screen already has.
pub type Running = Signal<HashSet<String>>;

/// Runs `game` to completion, holding it in `running` for as long as its
/// prefix has anything live in it.
///
/// Returns how long the session lasted, or `None` if it never started —
/// the caller decides whether that's worth recording as playtime.
pub async fn start(profile_id: String, game: InstalledGame, mut running: Running) -> Option<std::time::Duration> {
    let Some(executable) = crate::core::executable_for(&game) else {
        tracing::error!("{} has no launch executable", game.game_id);
        return None;
    };
    if !executable.is_file() {
        tracing::error!("{} is not on disk", executable.display());
        return None;
    }
    let Some(prefix) = crate::core::prefix_for(&profile_id, &game.game_id) else {
        tracing::error!("no prefix path for {}", game.game_id);
        return None;
    };

    let cloud_save_checkpoint = sync_cloud_saves_before_launch(&profile_id, &game).await;

    // `umu-<product id>` is umu's convention, and what protonfixes keys
    // its per-game workarounds off.
    let mut launch = Launch::new(executable, format!("umu-{}", game.game_id)).store("gog");
    launch.proton = crate::core::proton_for(&game);

    let session = match achv_runtime::start(&prefix, launch).await {
        Ok(session) => session,
        Err(err) => {
            tracing::error!("launch failed: {err}");
            return None;
        }
    };

    running.write().insert(game.game_id.clone());
    let started = std::time::Instant::now();
    // Reported, not propagated: the session is over either way, and the
    // running state has to be cleared regardless of how it ended.
    if let Err(err) = session.wait().await {
        tracing::warn!("session for {} ended badly: {err}", game.name);
    }
    running.write().remove(&game.game_id);

    sync_cloud_saves_after_exit(&profile_id, &game, cloud_save_checkpoint).await;
    record_last_played(&profile_id, &game);
    Some(started.elapsed())
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
async fn sync_cloud_saves_before_launch(profile_id: &str, game: &InstalledGame) -> Option<i64> {
    let Some(local_root) = game.cloud_save_path.as_deref() else {
        return game.cloud_save_last_sync;
    };
    match achv_gog::cloud_saves::sync(
        profile_id,
        &game.game_id,
        std::path::Path::new(local_root),
        game.cloud_save_last_sync,
        None,
    )
    .await
    {
        Ok(outcome) if outcome.action == achv_gog::cloud_saves::SyncAction::Conflict => {
            tracing::warn!(
                "cloud saves conflict for {} — playing with the local copy, resolve it from Propiedades",
                game.game_id
            );
            game.cloud_save_last_sync
        }
        Ok(outcome) => {
            if let Err(err) = crate::core::record_cloud_save_sync(profile_id, &game.game_id, outcome.last_sync) {
                tracing::warn!("could not record cloud saves sync for {}: {err}", game.game_id);
            }
            Some(outcome.last_sync)
        }
        Err(err) => {
            tracing::warn!("cloud saves pre-launch sync failed for {}: {err}", game.game_id);
            game.cloud_save_last_sync
        }
    }
}

/// Pushes local changes up once a session ends, mirroring
/// [`sync_cloud_saves_before_launch`]. `checkpoint` is that call's return
/// value, not `game.cloud_save_last_sync` — see its doc for why.
async fn sync_cloud_saves_after_exit(profile_id: &str, game: &InstalledGame, checkpoint: Option<i64>) {
    let Some(local_root) = game.cloud_save_path.as_deref() else {
        return;
    };
    match achv_gog::cloud_saves::sync(
        profile_id,
        &game.game_id,
        std::path::Path::new(local_root),
        checkpoint,
        None,
    )
    .await
    {
        Ok(outcome) if outcome.action == achv_gog::cloud_saves::SyncAction::Conflict => {
            tracing::warn!(
                "cloud saves conflict for {} after exit — resolve it from Propiedades",
                game.game_id
            );
        }
        Ok(outcome) => {
            if let Err(err) = crate::core::record_cloud_save_sync(profile_id, &game.game_id, outcome.last_sync) {
                tracing::warn!("could not record cloud saves sync for {}: {err}", game.game_id);
            }
        }
        Err(err) => tracing::warn!("cloud saves post-exit sync failed for {}: {err}", game.game_id),
    }
}

/// Ends a game's session.
///
/// Prefix-wide, which is now the same thing as game-wide: each game has
/// its own prefix, so stopping one can't reach another.
pub async fn stop(profile_id: String, game_id: String) {
    let Some(prefix) = crate::core::prefix_for(&profile_id, &game_id) else {
        return;
    };
    if let Err(err) = prefix.stop().await {
        tracing::error!("stop failed: {err}");
    }
}

/// Stops every game achv started.
///
/// Games outlive the app that launched them — umu execs into a container
/// and Proton detaches, so nothing here is a child process by the time it
/// matters. Closing achv without this leaves them running with no way
/// left to reach them from the UI.
pub async fn stop_everything() {
    achv_runtime::stop_all().await;
}

/// Stops everything and exits, for the signals a desktop actually sends —
/// a session logging out, `pkill`, Ctrl+C from a terminal.
///
/// Only the polite ones: `SIGKILL` can't be caught, so a hard kill still
/// leaves games running. Nothing can change that.
#[cfg(feature = "desktop")]
pub fn exit_with_games_on_signal() {
    dioxus::core::spawn_forever(async {
        let mut term = match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(signal) => signal,
            Err(err) => {
                tracing::warn!("cannot watch for SIGTERM: {err}");
                return;
            }
        };
        let mut interrupt = match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt()) {
            Ok(signal) => signal,
            Err(err) => {
                tracing::warn!("cannot watch for SIGINT: {err}");
                return;
            }
        };

        tokio::select! {
            _ = term.recv() => {}
            _ = interrupt.recv() => {}
        }
        stop_everything().await;
        std::process::exit(0);
    });
}

/// Throws a game's prefix away so umu builds a fresh one next launch.
///
/// The escape hatch for a prefix a game has wrecked — the reason each
/// game gets its own. Saves a game keeps inside its prefix go with it,
/// which is why this is never automatic.
pub async fn reset_prefix(profile_id: String, game_id: String) -> Result<(), String> {
    let Some(prefix) = crate::core::prefix_for(&profile_id, &game_id) else {
        return Err("no hay prefijo para este juego".to_string());
    };
    // Nothing may be running in it: deleting a live prefix out from under
    // Wine corrupts whatever it was writing.
    crate::achv_try!(prefix.stop().await.map_err(|err| err.to_string()));
    match tokio::fs::remove_dir_all(prefix.path()).await {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(format!("borrando el prefijo: {err}")),
    }
}

/// Stamps the manifest with when the session ended. Best-effort: a failed
/// write costs a sort order, not a game.
fn record_last_played(profile_id: &str, game: &InstalledGame) {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_secs() as i64);
    let Ok(library) = crate::core::library() else {
        return;
    };
    // Re-read rather than reusing the copy this launch started from: the
    // session may have outlasted an edit to the manifest.
    let mut current = library
        .read_manifest(profile_id, game.storefront, &game.game_id)
        .unwrap_or_else(|| game.clone());
    current.last_played = Some(now);
    if let Err(err) = library.write_manifest(&current) {
        tracing::warn!("could not record last played for {}: {err}", game.game_id);
    }
}
