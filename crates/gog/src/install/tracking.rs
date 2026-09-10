//! Process-wide install bookkeeping: which downloads are in flight, and
//! which (profile, game) pairs have been asked to stop — shared state
//! [`cancel`] needs regardless of which phase an install is in.

use std::collections::{HashMap, HashSet};

use tokio::sync::Mutex;

use super::download::Cancel;
use super::staging::staging_root;

pub(super) type InstallKey = (String, String);

/// In-flight installs, so [`cancel`] can stop their downloads. One
/// install per (profile, game), and one flag per install — every file it
/// is transferring shares that flag.
pub(super) fn active() -> &'static Mutex<HashMap<InstallKey, Cancel>> {
    static ACTIVE: std::sync::OnceLock<Mutex<HashMap<InstallKey, Cancel>>> = std::sync::OnceLock::new();
    ACTIVE.get_or_init(Default::default)
}

/// A cancel that arrives before `active()` has anything to reach: the UI
/// shows a Cancelar button the moment an install is queued, but the
/// session refresh and depot plan fetch it's waiting on can each take a
/// real amount of time, and neither has enqueued a download yet for
/// [`cancel`] to act on. Recorded here so `install` can honor it once it
/// reaches the point `active()` would otherwise start being useful,
/// instead of silently finishing an install the user already cancelled.
pub(super) fn cancel_requested() -> &'static Mutex<HashSet<InstallKey>> {
    static REQUESTED: std::sync::OnceLock<Mutex<HashSet<InstallKey>>> = std::sync::OnceLock::new();
    REQUESTED.get_or_init(Default::default)
}

/// Cancels an in-flight install for this (profile, product). A no-op when
/// nothing is running for that pair — safe to call defensively.
pub async fn cancel(profile_id: &str, product_id: &str) {
    let key: InstallKey = (profile_id.to_string(), product_id.to_string());
    // Set unconditionally, not just when `active()` came up empty: a
    // download-phase cancel below already stops the install on its own
    // (every in-flight file fails, which `install` treats as a failure),
    // but recording it here too costs nothing and means the early check in
    // `install` doesn't need to know which phase a given cancel landed in.
    cancel_requested().lock().await.insert(key.clone());
    if let Some(in_flight) = active().lock().await.remove(&key) {
        in_flight.cancel();
    }
    // Whatever had been transferred is worthless on its own, and leaving
    // it would be mistaken for progress by the next attempt.
    if let Ok(staging) = staging_root(profile_id, product_id) {
        tokio::fs::remove_dir_all(staging).await.ok();
    }
}

#[cfg(test)]
mod tests;
