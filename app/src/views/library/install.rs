//! Drives `decanter-gog`'s in-process install. The `installing` map is our
//! own guard against starting a second install for the same game while
//! one is in flight; `decanter-gog` keys its cancellable handles the same
//! way.
//!
//! Every function here runs as an app-level task (`cx.spawn` on the app,
//! detached), not a view's: an install outlives the button that started it.

use std::time::Duration;

use gpui::AsyncApp;
use rust_i18n::t;

use crate::state::{AppState, InstallStatus};

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

fn set_status(state: &AppState, key: &str, status: Option<InstallStatus>, cx: &mut AsyncApp) {
    state.background.update(cx, |background, cx| {
        match status {
            Some(status) => background.installing.insert(key.to_owned(), status),
            None => background.installing.remove(key),
        };
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

    let Ok(core_library) = crate::core::library_for_account(&account_id).map(|library| match &library_root {
        Some(root) => library.targeting(root),
        None => library,
    }) else {
        tracing::error!(account_id, product_id, "could not resolve library, install aborted");
        set_status(
            &state,
            &product_id,
            Some(InstallStatus::Failed(t!("library.install.no_library").into_owned())),
            cx,
        );
        return;
    };

    // A depot manifest can be thousands of tiny chunks — re-rendering on
    // every one falls behind the transfer badly, so progress is throttled
    // and rendering never becomes the bottleneck.
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<decanter_gog::Progress>();
    let install = decanter_gog::install(
        &core_library,
        &account_id,
        &product_id,
        parent_id.as_deref(),
        &title,
        branch.as_deref(),
        None,
        move |progress| {
            let _ = tx.send(progress);
        },
    );

    let mut last_ui_update = std::time::Instant::now()
        .checked_sub(UI_UPDATE_INTERVAL)
        .unwrap_or_else(std::time::Instant::now);
    tokio::pin!(install);
    let outcome = loop {
        tokio::select! {
            result = &mut install => break result,
            Some(progress) = rx.recv() => {
                if last_ui_update.elapsed() >= UI_UPDATE_INTERVAL {
                    set_status(&state, &product_id, Some(InstallStatus::Installing {
                        current_file: progress.current_file,
                        bytes_downloaded: progress.bytes_downloaded,
                        total_bytes: progress.total_bytes,
                        bytes_per_second: progress.bytes_per_second,
                    }), cx);
                    last_ui_update = std::time::Instant::now();
                }
            }
        }
    };

    if let Err(err) = outcome {
        tracing::error!(account_id, product_id, error = %err, "install failed");
        set_status(&state, &product_id, Some(InstallStatus::Failed(err.to_string())), cx);
        return;
    }
    tracing::info!(account_id, product_id, "install finished");

    // Each DLC reports under its own key, so the game's own row stops
    // showing progress once the game itself is down and each DLC shows
    // its own. They go one at a time: they share a destination directory.
    set_status(&state, &product_id, None, cx);
    refresh_install_state(&state, &account_id, &product_id, cx);
    let cached_art = state
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
        .unwrap_or_default();
    sync_steam_shortcut_for(&account_id, &product_id, cached_art).await;
    if let Some(game) = crate::core::installed_game(&account_id, &product_id) {
        crate::taskbar::sync_pc(&game).await;
    }

    for (dlc_id, dlc_title) in dlcs {
        tracing::info!(account_id, product_id, dlc_id, dlc_title, "dlc install started");
        set_status(&state, &dlc_id, Some(InstallStatus::Queued), cx);
        let outcome = decanter_gog::install(
            &core_library,
            &account_id,
            &dlc_id,
            Some(&product_id),
            &dlc_title,
            None,
            None,
            |_| {},
        )
        .await;
        match outcome {
            // A DLC that failed is worth surfacing, but it doesn't undo
            // the game that installed fine just before it.
            Err(err) => {
                tracing::error!(account_id, product_id, dlc_id, error = %err, "dlc install failed");
                set_status(&state, &dlc_id, Some(InstallStatus::Failed(err.to_string())), cx);
            }
            Ok(()) => {
                tracing::info!(account_id, product_id, dlc_id, "dlc install finished");
                set_status(&state, &dlc_id, None, cx);
            }
        }
    }
    refresh_install_state(&state, &account_id, &product_id, cx);
}

pub async fn cancel(account_id: &str, product_id: &str) {
    tracing::info!(account_id, product_id, "cancelling install");
    decanter_gog::cancel(account_id, product_id).await;
}

/// Cancels and clears the row: cancelling only stops the transfer, and
/// nothing else would clear it.
pub async fn cancel_and_forget(account_id: String, product_id: String, state: AppState, cx: &mut AsyncApp) {
    cancel(&account_id, &product_id).await;
    set_status(&state, &product_id, None, cx);
    refresh_install_state(&state, &account_id, &product_id, cx);
}

pub async fn uninstall(
    account_id: String,
    product_id: String,
    state: AppState,
    cx: &mut AsyncApp,
) -> Result<(), String> {
    tracing::info!(account_id, product_id, "uninstall started");
    let core_library = crate::achv_try!(
        crate::core::library_for_account(&account_id)
            .map_err(|err| t!("library.install.library_error", error = err).into_owned())
    );
    crate::achv_try!(
        decanter_gog::uninstall(&core_library, &account_id, &product_id)
            .await
            .map_err(|err| err.to_string())
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

// Nothing to clean up here any more: a failed install never reaches the
// library. Files land in a staging directory outside it and only move
// across once every byte has arrived, and `decanter-gog` deletes that
// staging directory on any failure or cancel.
