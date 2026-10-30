//! Headless install/cancel — the same `decanter-gog` functions the GUI's
//! install button and cancel action call, run here directly so their real
//! behavior (and any errors) can be observed independent of the GUI.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use anyhow::Context;

/// How often a download waiting for another decanter process says it still is.
const WAITING_NOTICE: Duration = Duration::from_secs(15);

pub struct Target<'args> {
    pub account_id: &'args str,
    pub product_id: &'args str,
    pub title: &'args str,
    pub branch: Option<&'args str>,
    pub build_id: Option<&'args str>,
}

/// One download runs at a time across decanter processes (#1320): with the
/// app downloading, this waits for its turn, saying so, or with `no_wait`
/// exits at once.
pub async fn install(target: Target<'_>, no_wait: bool) -> anyhow::Result<()> {
    let Target {
        account_id,
        product_id,
        title,
        branch,
        build_id,
    } = target;
    tracing::info!(account_id, product_id, title, branch, build_id, "install started");
    let library = crate::achv_try!(
        crate::core::library_for_account(account_id).map_err(|err| anyhow::anyhow!("resolving library: {err}"))
    );
    if decanter_gog::download_slot_busy(&library) {
        if no_wait {
            anyhow::bail!(
                "another Decanter process is downloading into this library; run this again once it finishes, \
                 or without --no-wait to wait for its turn"
            );
        }
        tracing::info!(
            account_id,
            product_id,
            "another Decanter process is downloading, waiting for its turn"
        );
    }
    let events = Arc::new(AtomicU64::new(0));
    let counted = Arc::clone(&events);
    let install = decanter_gog::install(
        &library,
        account_id,
        product_id,
        None,
        title,
        branch,
        build_id,
        move |progress| {
            let seen = counted.fetch_add(1, Ordering::Relaxed) + 1;
            if seen % 200 == 1 {
                tracing::info!(
                    file = %progress.current_file,
                    downloaded = progress.bytes_downloaded,
                    total = ?progress.total_bytes,
                    rate = progress.bytes_per_second,
                    events = seen,
                    "progress"
                );
            }
        },
    );
    tokio::pin!(install);
    let mut notice = tokio::time::interval(WAITING_NOTICE);
    notice.tick().await;
    let result = loop {
        tokio::select! {
            result = &mut install => break result.context("installing"),
            _ = notice.tick() => {
                if events.load(Ordering::Relaxed) == 0 && decanter_gog::download_slot_busy(&library) {
                    tracing::info!(account_id, product_id, "still waiting for another Decanter process's download to finish");
                }
            }
        }
    };
    match &result {
        Ok(()) => {
            tracing::info!(account_id, product_id, "install finished");
            sync_steam_shortcut_after_install(&library, account_id, product_id).await;
        }
        Err(err) => tracing::error!(account_id, product_id, %err, "install failed"),
    }
    result
}

/// Best-effort, same rule as the GUI install path's own hook in
/// `ui/src/views/library/install.rs`: never fails this CLI command over
/// it, just logs and moves on.
async fn sync_steam_shortcut_after_install(library: &decanter_core::Library, account_id: &str, product_id: &str) {
    let Ok(db) = crate::core::db() else {
        return;
    };
    let Some(game) = library.read_manifest(account_id, decanter_core::Storefront::Gog, product_id) else {
        return;
    };
    let Some(install_dir) = library.install_dir(&game.install_dir) else {
        return;
    };
    // No cached catalog data on this headless path — an extra fetch, but
    // this command's own doc comment already calls it a diagnostic one.
    let catalog_game = decanter_gog::find_game(library, account_id, product_id).await.ok();
    let (cover_url, background_url, icon_url) = catalog_game
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

/// For clearing out an install left running by a client that died
/// mid-transfer — installing again for the same identity just races
/// whatever this cancels.
pub async fn cancel(account_id: &str, product_id: &str) {
    tracing::info!(account_id, product_id, "cancelling install");
    decanter_gog::cancel(account_id, product_id).await;
}
