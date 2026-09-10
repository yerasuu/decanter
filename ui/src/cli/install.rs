//! Headless install/cancel — the same `achv-gog` functions the GUI's
//! install button and cancel action call, run here directly so their real
//! behavior (and any errors) can be observed independent of the Dioxus app.

use anyhow::Context;

pub async fn install(
    account_id: &str,
    product_id: &str,
    title: &str,
    branch: Option<&str>,
    build_id: Option<&str>,
) -> anyhow::Result<()> {
    tracing::info!(account_id, product_id, title, branch, build_id, "install started");
    let mut events = 0u64;
    let result = achv_gog::install(account_id, product_id, None, title, branch, build_id, move |progress| {
        events += 1;
        if events % 200 == 1 {
            tracing::info!(
                file = %progress.current_file,
                downloaded = progress.bytes_downloaded,
                total = ?progress.total_bytes,
                rate = progress.bytes_per_second,
                events,
                "progress"
            );
        }
    })
    .await
    .context("installing");
    match &result {
        Ok(()) => tracing::info!(account_id, product_id, "install finished"),
        Err(err) => tracing::error!(account_id, product_id, %err, "install failed"),
    }
    result
}

/// For clearing out an install left running by a client that died
/// mid-transfer — installing again for the same identity just races
/// whatever this cancels.
pub async fn cancel(account_id: &str, product_id: &str) {
    tracing::info!(account_id, product_id, "cancelling install");
    achv_gog::cancel(account_id, product_id).await;
}
