//! The retention safety net: unresolved reports past `retention_days` and resolved ones past
//! `resolved_retention_days` go, with their attachments, and so do blobs and index entries nothing refers to any
//! more. Stale transactions are recovered more often, since a lock they hold blocks every upload.

use std::time::Duration;

use chrono::{Days, Utc};

use crate::app::AppState;

const EVERY: Duration = Duration::from_hours(24);
const RECOVER_EVERY: Duration = Duration::from_mins(1);

fn cutoff_day(days: u32) -> Option<String> {
    Utc::now()
        .date_naive()
        .checked_sub_days(Days::new(u64::from(days)))
        .map(|day| day.format("%Y-%m-%d").to_string())
}

pub async fn run_once(state: &AppState) {
    let Some(unresolved_cutoff) = cutoff_day(state.config.retention_days) else {
        return;
    };
    let resolved_cutoff = match state.config.resolved_retention_days {
        0 => None,
        days => cutoff_day(days),
    };
    let today = crate::app::today();
    for (product, store) in state.stores.iter() {
        let product = product.name();
        match store
            .sweep(&today, &unresolved_cutoff, resolved_cutoff.as_deref())
            .await
        {
            Ok(swept) => {
                if swept.unresolved + swept.resolved > 0 {
                    state.index_cache.clear();
                }
                tracing::info!(
                    product,
                    unresolved = swept.unresolved,
                    resolved = swept.resolved,
                    attachments = swept.attachments,
                    orphans = swept.orphans,
                    repaired = swept.repaired,
                    "retention sweep"
                );
            }
            Err(error) => tracing::error!(product, %error, "retention sweep failed"),
        }
    }
}

pub async fn daily(state: AppState) {
    loop {
        run_once(&state).await;
        tokio::time::sleep(EVERY).await;
    }
}

pub async fn recover(state: AppState) {
    loop {
        tokio::time::sleep(RECOVER_EVERY).await;
        for (product, store) in state.stores.iter() {
            let product = product.name();
            match store.recover().await {
                Ok(0) => {}
                Ok(recovered) => tracing::warn!(product, recovered, "stale transactions recovered"),
                Err(error) => tracing::error!(product, %error, "transaction recovery failed"),
            }
        }
        // A `user` command that died mid-transaction leaves its locks in the accounts store too.
        match state.accounts.store().recover().await {
            Ok(0) => {}
            Ok(recovered) => tracing::warn!(store = "accounts", recovered, "stale transactions recovered"),
            Err(error) => tracing::error!(store = "accounts", %error, "transaction recovery failed"),
        }
    }
}
