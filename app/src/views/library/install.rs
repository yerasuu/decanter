//! Drives `decanter-gog`'s in-process downloads. The `installing` map is
//! our own guard against starting a second install for the same game while
//! one is in flight; `decanter-gog` keys its cancellable handles the same
//! way, by (account, product). Every download, wherever it is started
//! (library, updates, DLC, branches, versions, repair), goes through
//! [`track`] so Descargas lists it and can cancel it.
//!
//! Every function here runs as an app-level task (`cx.spawn` on the app,
//! detached), not a view's: an install outlives the button that started it.

use std::future::Future;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use gpui::AsyncApp;
use tokio::sync::mpsc::UnboundedReceiver;

use crate::i18n::Text;
use crate::state::{AppState, Download, DownloadKey, DownloadKind, InstallStatus};

const UI_UPDATE_INTERVAL: Duration = Duration::from_millis(150);

pub struct InstallRequest {
    pub account_id: String,
    pub product_id: String,
    pub parent_id: Option<String>,
    pub title: String,
    /// `None` is mainline. Ignored when `parent_id` is `Some` — a DLC's
    /// branch always comes from the base game it belongs to (see
    /// `decanter_gog::install`'s own doc).
    pub branch: Option<String>,
    /// DLC to install straight after, as `(product id, title)`. The way
    /// Steam does it: what you ticked before hitting install arrives with
    /// the game, not as a second thing to go and do.
    pub dlcs: Vec<(String, String)>,
    /// The library folder a new install goes in; `None` is the profile's
    /// main one. An update or DLC goes wherever the game already is.
    pub library_root: Option<std::path::PathBuf>,
}

/// The row a tracked download shows in Descargas.
pub struct DownloadJob {
    pub key: DownloadKey,
    pub title: String,
    pub kind: DownloadKind,
}

/// How a tracked download ended. A cancel or a pause is the user's own
/// doing or shown on its row, so the caller shows nothing for it.
pub enum Ended<T> {
    Done(T),
    Cancelled,
    /// Stopped with its data kept; the row says why.
    Paused {
        reason: decanter_gog::PauseReason,
        bytes_done: u64,
    },
    Failed(Text),
}

/// A fresh `Queued` row, for a caller that wants it on screen before the
/// task that [`track`]s it gets to run.
pub fn queued(title: String, kind: DownloadKind) -> Download {
    Download {
        title,
        kind,
        run: next_run(),
        status: InstallStatus::Queued,
    }
}

fn next_run() -> u64 {
    static RUN: AtomicU64 = AtomicU64::new(0);
    RUN.fetch_add(1, Ordering::Relaxed)
}

/// The callback to hand `decanter_gog`'s download and the receiver to hand
/// [`track`] with it.
pub fn progress_channel() -> (
    impl FnMut(decanter_gog::Progress) + Send + 'static,
    UnboundedReceiver<decanter_gog::Progress>,
) {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    // A closed receiver only means the row is gone; the download goes on.
    (
        move |progress| {
            let _ = tx.send(progress);
        },
        rx,
    )
}

/// Shows `work` as a row in Descargas, fed by `progress`, until it ends:
/// every GOG download goes through here, so each one can be watched and
/// cancelled the same way. A finished or cancelled download drops its row;
/// a failed one leaves the failure on it until dismissed.
pub async fn track<T>(
    state: &AppState,
    job: DownloadJob,
    mut progress: UnboundedReceiver<decanter_gog::Progress>,
    work: impl Future<Output = anyhow::Result<T>>,
    cx: &mut AsyncApp,
) -> Ended<T> {
    let DownloadJob { key, title, kind } = job;
    let run = next_run();
    state.background.update(cx, |background, cx| {
        background.installing.insert(
            key.clone(),
            Download {
                title,
                kind,
                run,
                status: InstallStatus::Queued,
            },
        );
        cx.notify();
    });

    // A depot manifest can be thousands of tiny chunks — re-rendering on
    // every one falls behind the transfer badly, so progress is throttled
    // and rendering never becomes the bottleneck.
    let mut last_ui_update = Instant::now()
        .checked_sub(UI_UPDATE_INTERVAL)
        .unwrap_or_else(Instant::now);
    tokio::pin!(work);
    let outcome = loop {
        tokio::select! {
            result = &mut work => break result,
            Some(update) = progress.recv() => {
                if last_ui_update.elapsed() >= UI_UPDATE_INTERVAL {
                    report(state, &key, run, update, cx);
                    last_ui_update = Instant::now();
                }
            }
        }
    };

    let was_cancelled = state.background.update(cx, |background, _| {
        background
            .installing
            .get(&key)
            .is_some_and(|row| row.run == run && matches!(row.status, InstallStatus::Cancelling { .. }))
    });
    let ended = ended(&key, outcome, was_cancelled);
    state.background.update(cx, |background, cx| {
        if background.installing.get(&key).is_none_or(|row| row.run != run) {
            return;
        }
        match &ended {
            Ended::Failed(text) => {
                if let Some(row) = background.installing.get_mut(&key) {
                    row.status = InstallStatus::Failed(text.clone());
                }
            }
            Ended::Paused { reason, bytes_done } => {
                if let Some(row) = background.installing.get_mut(&key) {
                    let total_bytes = if let InstallStatus::Installing { total_bytes, .. } = row.status {
                        total_bytes
                    } else {
                        None
                    };
                    row.status = InstallStatus::Paused {
                        reason: *reason,
                        bytes_done: *bytes_done,
                        total_bytes,
                    };
                }
            }
            Ended::Done(_) | Ended::Cancelled => {
                background.installing.remove(&key);
            }
        }
        if background.cancel_confirm.as_ref() == Some(&key) {
            background.cancel_confirm = None;
        }
        cx.notify();
    });
    ended
}

fn ended<T>(key: &DownloadKey, outcome: anyhow::Result<T>, was_cancelled: bool) -> Ended<T> {
    let err = match outcome {
        Ok(value) => return Ended::Done(value),
        Err(err) => err,
    };
    // Whatever stopped a download the user had asked to stop, its staging
    // is gone like a cancel's, so it reads as one.
    if was_cancelled || err.is::<decanter_gog::Cancelled>() {
        tracing::info!(
            account_id = key.account_id,
            product_id = key.product_id,
            "download cancelled"
        );
        return Ended::Cancelled;
    }
    if let Some(stop) = err.downcast_ref::<decanter_gog::Paused>() {
        tracing::info!(
            account_id = key.account_id,
            product_id = key.product_id,
            reason = ?stop.reason,
            bytes_done = stop.bytes_done,
            "download paused"
        );
        return Ended::Paused {
            reason: stop.reason,
            bytes_done: stop.bytes_done,
        };
    }
    tracing::error!(
        account_id = key.account_id,
        product_id = key.product_id,
        error = %err,
        "download failed"
    );
    Ended::Failed(Text::verbatim(&err))
}

fn report(state: &AppState, key: &DownloadKey, run: u64, progress: decanter_gog::Progress, cx: &mut AsyncApp) {
    state.background.update(cx, |background, cx| {
        // A cancelling row keeps saying so until the download has stopped.
        if let Some(row) = background.installing.get_mut(key)
            && row.run == run
            && !matches!(row.status, InstallStatus::Cancelling { .. })
        {
            row.status = InstallStatus::Installing {
                current_file: progress.current_file,
                bytes_downloaded: progress.bytes_downloaded,
                total_bytes: progress.total_bytes,
                bytes_per_second: progress.bytes_per_second,
            };
            cx.notify();
        }
    });
}

/// The confirmed cancel of a paused download: nothing is running it to
/// delete its data and drop its row, so this does.
pub async fn discard_paused(key: DownloadKey, state: AppState, cx: &mut AsyncApp) {
    let outcome = match crate::core::library_for_account(&key.account_id) {
        Ok(library) => decanter_gog::discard(&library, &key.account_id, &key.product_id)
            .await
            .inspect_err(|err| {
                tracing::error!(
                    account_id = key.account_id,
                    product_id = key.product_id,
                    error = %err,
                    "deleting a paused download failed"
                );
            })
            .map_err(Text::verbatim),
        Err(err) => Err(Text::new("library.install.library_error").text("error", err.localized())),
    };
    state.background.update(cx, |background, cx| {
        match outcome {
            Ok(()) => {
                background.installing.remove(&key);
            }
            Err(text) => {
                if let Some(row) = background.installing.get_mut(&key) {
                    row.status = InstallStatus::Failed(text);
                }
            }
        }
        cx.notify();
    });
}

/// Lists the downloads a previous run left unfinished as paused rows, so
/// what they keep on disk is visible and can be cancelled; installing the
/// game again carries on from them.
pub async fn list_unfinished(profile_id: String, state: AppState, cx: &mut AsyncApp) {
    let library = match crate::core::library_for_profile(&profile_id) {
        Ok(library) => library,
        Err(err) => {
            tracing::warn!(profile_id, %err, "could not resolve the library to look for unfinished downloads");
            return;
        }
    };
    let unfinished = decanter_gog::unfinished_downloads(&library).await;
    state.background.update(cx, |background, cx| {
        for download in unfinished {
            // Another decanter process (the CLI) is running it; this one
            // has nothing to show for it.
            if download.in_use {
                tracing::info!(
                    account_id = download.account_id,
                    product_id = download.product_id,
                    "unfinished download is running in another process"
                );
                continue;
            }
            let kind = if download.is_repair {
                DownloadKind::Repair
            } else if download.parent_id.is_some() {
                DownloadKind::Dlc
            } else if library
                .read_manifest(
                    &download.account_id,
                    decanter_core::Storefront::Gog,
                    &download.product_id,
                )
                .is_none()
            {
                DownloadKind::Install
            } else if download.build_id.is_some() {
                DownloadKind::Rollback
            } else {
                DownloadKind::Update
            };
            let mut row = queued(download.title, kind);
            row.status = InstallStatus::Paused {
                reason: download.reason,
                bytes_done: download.bytes_done,
                total_bytes: Some(download.total_bytes),
            };
            background
                .installing
                .entry(DownloadKey::new(&download.account_id, &download.product_id))
                .or_insert(row);
        }
        cx.notify();
    });
}

fn refresh_install_state(state: &AppState, account_id: &str, product_id: &str, cx: &mut AsyncApp) {
    state.library.update(cx, |library, cx| {
        library.refresh_install_state(account_id, product_id, cx);
    });
}

/// Runs until the install finishes or fails. A failure surfaces as
/// [`InstallStatus::Failed`] in the action bar's red text.
pub async fn install(request: InstallRequest, state: AppState, cx: &mut AsyncApp) {
    let InstallRequest {
        account_id,
        product_id,
        parent_id,
        title,
        branch,
        dlcs,
        library_root,
    } = request;
    tracing::info!(
        account_id,
        product_id,
        title,
        branch = branch.as_deref(),
        dlc_count = dlcs.len(),
        "install started"
    );
    let key = DownloadKey::new(&account_id, &product_id);
    let kind = if parent_id.is_some() {
        DownloadKind::Dlc
    } else {
        DownloadKind::Install
    };

    let Ok(core_library) = crate::core::library_for_account(&account_id).map(|library| match &library_root {
        Some(root) => library.targeting(root),
        None => library,
    }) else {
        tracing::error!(account_id, product_id, "could not resolve library, install aborted");
        state.background.update(cx, |background, cx| {
            let mut row = queued(title, kind);
            row.status = InstallStatus::Failed(Text::new("library.install.no_library"));
            background.installing.insert(key, row);
            cx.notify();
        });
        return;
    };

    let (on_progress, progress) = progress_channel();
    let install = decanter_gog::install(
        &core_library,
        &account_id,
        &product_id,
        parent_id.as_deref(),
        &title,
        branch.as_deref(),
        None,
        on_progress,
    );
    let job = DownloadJob {
        key,
        title: title.clone(),
        kind,
    };
    if !matches!(track(&state, job, progress, install, cx).await, Ended::Done(())) {
        refresh_install_state(&state, &account_id, &product_id, cx);
        return;
    }
    tracing::info!(account_id, product_id, "install finished");

    refresh_install_state(&state, &account_id, &product_id, cx);
    let cached_art = cached_art(&state, &product_id, cx);
    sync_steam_shortcut_for(&account_id, &product_id, cached_art).await;
    if let Some(game) = crate::core::installed_game(&account_id, &product_id) {
        crate::taskbar::sync_pc(&game).await;
    }

    install_dlcs(&core_library, &account_id, &product_id, dlcs, &state, cx).await;
    refresh_install_state(&state, &account_id, &product_id, cx);
}

fn cached_art(state: &AppState, product_id: &str, cx: &AsyncApp) -> (Option<String>, Option<String>, Option<String>) {
    state
        .library
        .read_with(cx, |library, _| {
            library
                .games
                .iter()
                .find(|entry| entry.game.id == product_id)
                .map(|entry| {
                    (
                        entry.game.cover_url.clone(),
                        entry.game.background_url.clone(),
                        entry.game.icon_url.clone(),
                    )
                })
        })
        .unwrap_or_default()
}

/// Each DLC reports under its own key, so the game's own row is gone once
/// the game itself is down and each DLC shows its own. They go one at a
/// time: they share a destination directory.
async fn install_dlcs(
    core_library: &decanter_core::Library,
    account_id: &str,
    product_id: &str,
    dlcs: Vec<(String, String)>,
    state: &AppState,
    cx: &mut AsyncApp,
) {
    for (dlc_id, dlc_title) in dlcs {
        tracing::info!(account_id, product_id, dlc_id, dlc_title, "dlc install started");
        let (on_progress, progress) = progress_channel();
        let install = decanter_gog::install(
            core_library,
            account_id,
            &dlc_id,
            Some(product_id),
            &dlc_title,
            None,
            None,
            on_progress,
        );
        let job = DownloadJob {
            key: DownloadKey::new(account_id, &dlc_id),
            title: dlc_title.clone(),
            kind: DownloadKind::Dlc,
        };
        // A DLC that failed stays on its own row, but it doesn't undo the
        // game that installed fine just before it.
        if matches!(track(state, job, progress, install, cx).await, Ended::Done(())) {
            tracing::info!(account_id, product_id, dlc_id, "dlc install finished");
        }
    }
}

pub async fn uninstall(account_id: String, product_id: String, state: AppState, cx: &mut AsyncApp) -> Result<(), Text> {
    tracing::info!(account_id, product_id, "uninstall started");
    let core_library = crate::achv_try!(
        crate::core::library_for_account(&account_id)
            .map_err(|err| Text::new("library.install.library_error").text("error", err.localized()))
    );
    crate::achv_try!(
        decanter_gog::uninstall(&core_library, &account_id, &product_id)
            .await
            .map_err(Text::verbatim)
            // Bubbles up to the confirm step's red error text — this is
            // where it becomes user-visible.
            .inspect_err(|err| tracing::error!(account_id, product_id, error = %err, "uninstall failed"))
    );
    tracing::info!(account_id, product_id, "uninstall finished");
    refresh_install_state(&state, &account_id, &product_id, cx);
    remove_steam_shortcut_for(&account_id, &product_id).await;
    crate::taskbar::remove_pc(&account_id, decanter_core::Storefront::Gog, &product_id).await;
    Ok(())
}

/// Best-effort: exports (or re-syncs) this game's Steam shortcut after a
/// successful install. Never fails the install over it — a missing Steam
/// install, an unresolvable local Steam user, or a failed artwork
/// download all just cost the Steam entry (or one image on it), logged
/// and moved past.
async fn sync_steam_shortcut_for(
    account_id: &str,
    product_id: &str,
    (cover_url, background_url, icon_url): (Option<String>, Option<String>, Option<String>),
) {
    let Ok(db) = crate::core::db() else {
        return;
    };
    // Writes into the real Steam user's own config files, so it's worth
    // an easy way to turn off — see the Steam export toggle in Ajustes.
    if !db.steam_export_enabled().unwrap_or(true) {
        tracing::debug!(account_id, product_id, "steam export disabled in settings, skipping");
        return;
    }
    let Ok(core_library) = crate::core::library_for_account(account_id) else {
        return;
    };
    let Some(game) = core_library.read_manifest(account_id, decanter_core::Storefront::Gog, product_id) else {
        tracing::debug!(
            account_id,
            product_id,
            "no manifest to export to steam right after install"
        );
        return;
    };
    let Some(install_dir) = core_library.install_dir(&game.install_dir) else {
        return;
    };
    if let Err(err) = decanter_gog::sync_steam_shortcut(
        &db,
        account_id,
        &game,
        &install_dir,
        cover_url.as_deref(),
        background_url.as_deref(),
        icon_url.as_deref(),
    )
    .await
    {
        tracing::warn!(account_id, product_id, %err, "failed to export steam shortcut");
    }
}

/// Best-effort counterpart for uninstall — same "never fail the caller
/// over this" rule as [`sync_steam_shortcut_for`].
async fn remove_steam_shortcut_for(account_id: &str, product_id: &str) {
    let Ok(db) = crate::core::db() else {
        return;
    };
    if let Err(err) = decanter_gog::remove_steam_shortcut(&db, account_id, product_id).await {
        tracing::warn!(account_id, product_id, %err, "failed to remove steam shortcut");
    }
}

// Nothing to clean up here: a failed install never reaches the library.
// Files land in a staging directory outside it and only move across once
// every byte has arrived. `decanter-gog` keeps that directory, with its
// journal, until the download finishes or a confirmed cancel deletes it.
