//! Drives `decanter-gog`'s in-process install. The `installing` map is still
//! our own guard against starting a second install for the same game
//! while one is in flight; `decanter-gog` keys its cancellable handles the
//! same way.

use std::collections::HashSet;
use std::time::Duration;

use dioxus::prelude::*;

use crate::background::InstallStatus;

const UI_UPDATE_INTERVAL: Duration = Duration::from_millis(150);

/// Runs until the install finishes or fails. A failure falls through to
/// best-effort cleanup: a partial install leaves files behind with no
/// `InstallRecord` to tie them to, so nothing else would ever remove them.
pub(super) async fn install(
    account_id: String,
    product_id: String,
    parent_id: Option<String>,
    title: String,
    // `None` is mainline. Ignored when `parent_id` is `Some` — a DLC's
    // branch always comes from the base game it belongs to, not the
    // caller (see `decanter_gog::install`'s own doc).
    branch: Option<String>,
    // DLC to install straight after, as `(product id, title)`. The way
    // Steam does it: what you ticked before hitting install arrives with
    // the game, not as a second thing to go and do.
    dlcs: Vec<(String, String)>,
    product_id_key: String,
    mut installing: Signal<std::collections::HashMap<String, InstallStatus>>,
    library: crate::gog_library::GogLibrary,
) {
    // A depot manifest can be thousands of tiny chunks — writing to the
    // Dioxus signal (and the render it triggers) on every single one
    // falls behind the transfer badly. Progress is throttled so rendering
    // never becomes the bottleneck.
    tracing::info!(
        account_id,
        product_id,
        title,
        branch = branch.as_deref(),
        dlc_count = dlcs.len(),
        "install started"
    );

    let Ok(core_library) = crate::core::library_for_account(&account_id) else {
        tracing::error!(account_id, product_id, "could not resolve library, install aborted");
        installing.write().insert(
            product_id_key,
            InstallStatus::Failed("no se pudo resolver la biblioteca".to_string()),
        );
        return;
    };

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

    let mut touched_files: HashSet<String> = HashSet::new();
    let mut last_ui_update = std::time::Instant::now().checked_sub(UI_UPDATE_INTERVAL).unwrap();
    let key = product_id_key.clone();

    tokio::pin!(install);
    let outcome = loop {
        tokio::select! {
            result = &mut install => break result,
            Some(progress) = rx.recv() => {
                touched_files.insert(progress.current_file.clone());
                if last_ui_update.elapsed() >= UI_UPDATE_INTERVAL {
                    installing.write().insert(
                        key.clone(),
                        InstallStatus::Installing {
                            current_file: progress.current_file,
                            bytes_downloaded: progress.bytes_downloaded,
                            total_bytes: progress.total_bytes,
                            bytes_per_second: progress.bytes_per_second,
                        },
                    );
                    last_ui_update = std::time::Instant::now();
                }
            }
        }
    };

    let _ = touched_files;
    if let Err(err) = outcome {
        // Surfaces as `InstallStatus::Failed` in the action bar's red text —
        // this is the point the failure becomes visible to the user.
        tracing::error!(account_id, product_id, error = %err, "install failed");
        installing
            .write()
            .insert(product_id_key, InstallStatus::Failed(err.to_string()));
        return;
    }
    tracing::info!(account_id, product_id, "install finished");

    // Each DLC reports under its own key, so the game's own row stops
    // showing progress once the game itself is down and each DLC shows
    // its own. They go one at a time: they share a destination directory.
    installing.write().remove(&product_id_key);
    library.refresh_install_state(&account_id, &product_id);
    sync_steam_shortcut_for(&account_id, &product_id, &library).await;

    for (dlc_id, dlc_title) in dlcs {
        tracing::info!(account_id, product_id, dlc_id, dlc_title, "dlc install started");
        installing.write().insert(dlc_id.clone(), InstallStatus::Queued);
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
                installing
                    .write()
                    .insert(dlc_id, InstallStatus::Failed(err.to_string()));
            }
            Ok(()) => {
                tracing::info!(account_id, product_id, dlc_id, "dlc install finished");
                installing.write().remove(&dlc_id);
            }
        }
    }
    library.refresh_install_state(&account_id, &product_id);
}

pub(crate) async fn cancel(account_id: String, product_id: String) {
    tracing::info!(account_id, product_id, "cancelling install");
    decanter_gog::cancel(&account_id, &product_id).await;
}

pub(super) async fn uninstall(
    account_id: String,
    product_id: String,
    library: crate::gog_library::GogLibrary,
) -> Result<(), String> {
    tracing::info!(account_id, product_id, "uninstall started");
    let core_library = crate::achv_try!(
        crate::core::library_for_account(&account_id).map_err(|err| format!("resolviendo la biblioteca: {err}"))
    );
    crate::achv_try!(
        decanter_gog::uninstall(&core_library, &account_id, &product_id)
            .await
            .map_err(|err| err.to_string())
            // Bubbles up to the confirm dialog's red error text — this is
            // where it becomes user-visible.
            .inspect_err(|err| tracing::error!(account_id, product_id, error = %err, "uninstall failed"))
    );
    tracing::info!(account_id, product_id, "uninstall finished");
    library.refresh_install_state(&account_id, &product_id);
    remove_steam_shortcut_for(&account_id, &product_id).await;
    Ok(())
}

/// Best-effort: exports (or re-syncs) this game's Steam shortcut after a
/// successful install. Never fails the install over it — a missing Steam
/// install, an unresolvable local Steam user, or a failed artwork
/// download all just cost the Steam entry (or one image on it), logged
/// and moved past.
async fn sync_steam_shortcut_for(account_id: &str, product_id: &str, library: &crate::gog_library::GogLibrary) {
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

    let cached_game = library
        .games
        .peek()
        .iter()
        .find(|entry| entry.game.id == product_id)
        .map(|entry| entry.game.clone());
    let (cover_url, background_url, icon_url) = cached_game
        .map(|game| (game.cover_url, game.background_url, game.icon_url))
        .unwrap_or_default();

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
