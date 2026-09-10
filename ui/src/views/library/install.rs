//! Drives `achv-gog`'s in-process install. The `installing` map is still
//! our own guard against starting a second install for the same game
//! while one is in flight; `achv-gog` keys its cancellable handles the
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
    // caller (see `achv_gog::install`'s own doc).
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

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<achv_gog::Progress>();
    let install = achv_gog::install(
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

    for (dlc_id, dlc_title) in dlcs {
        tracing::info!(account_id, product_id, dlc_id, dlc_title, "dlc install started");
        installing.write().insert(dlc_id.clone(), InstallStatus::Queued);
        let outcome = achv_gog::install(&account_id, &dlc_id, Some(&product_id), &dlc_title, None, None, |_| {}).await;
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
    achv_gog::cancel(&account_id, &product_id).await;
}

pub(super) async fn uninstall(
    account_id: String,
    product_id: String,
    library: crate::gog_library::GogLibrary,
) -> Result<(), String> {
    tracing::info!(account_id, product_id, "uninstall started");
    crate::achv_try!(
        achv_gog::uninstall(&account_id, &product_id)
            .await
            .map_err(|err| err.to_string())
            // Bubbles up to the confirm dialog's red error text — this is
            // where it becomes user-visible.
            .inspect_err(|err| tracing::error!(account_id, product_id, error = %err, "uninstall failed"))
    );
    tracing::info!(account_id, product_id, "uninstall finished");
    library.refresh_install_state(&account_id, &product_id);
    Ok(())
}

// Nothing to clean up here any more: a failed install never reaches the
// library. Files land in a staging directory outside it and only move
// across once every byte has arrived, and `achv-gog` deletes that
// staging directory on any failure or cancel.
