//! Process-wide install bookkeeping: which downloads are in flight, and
//! which (profile, game) pairs have been asked to stop — shared state
//! [`cancel`] and [`pause`] need regardless of which phase an install is in.

use std::collections::HashMap;

use tokio::sync::Mutex;

use super::download::Cancel;

pub(super) type InstallKey = (String, String);

/// In-flight installs, so [`cancel`] can stop their downloads. One
/// install per (profile, game), and one flag per install — every file it
/// is transferring shares that flag.
pub(super) fn active() -> &'static Mutex<HashMap<InstallKey, Cancel>> {
    static ACTIVE: std::sync::OnceLock<Mutex<HashMap<InstallKey, Cancel>>> = std::sync::OnceLock::new();
    ACTIVE.get_or_init(Default::default)
}

/// A stop that arrives before `active()` has anything to reach: the UI
/// shows Cancelar (and, later, Pausar) the moment an install is queued, but
/// the session refresh and depot plan fetch it's waiting on can each take a
/// real amount of time, and neither has enqueued a download yet for
/// [`cancel`] or [`pause`] to act on. Recorded here so `install` honors it
/// once its journal is open, instead of silently downloading anyway.
pub(super) fn stop_requested() -> &'static Mutex<HashMap<InstallKey, Stop>> {
    static REQUESTED: std::sync::OnceLock<Mutex<HashMap<InstallKey, Stop>>> = std::sync::OnceLock::new();
    REQUESTED.get_or_init(Default::default)
}

/// What the user asked a download to do instead of finishing.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Stop {
    /// Confirmed: everything it downloaded is deleted.
    Cancel,
    /// Kept on disk, to carry on from later.
    Pause,
}

/// Claims `key` for a fresh run: a second download of the same product
/// would enqueue a second writer for every one of its files, so whatever is
/// already running is paused first (its journal is what the new run carries
/// on from), and any stop asked for earlier is forgotten so it doesn't land
/// on the run about to start.
pub(super) async fn claim(account_id: &str, product_id: &str) {
    let key: InstallKey = (account_id.to_owned(), product_id.to_owned());
    if let Some(in_flight) = active().lock().await.remove(&key) {
        in_flight.pause();
    }
    stop_requested().lock().await.remove(&key);
}

/// The stop asked for while `key` was still being planned, before any
/// download existed to reach. Consumes it.
pub(super) async fn stop_while_queued(key: &InstallKey) -> Option<Stop> {
    stop_requested().lock().await.remove(key)
}

/// Cancels an install for this (profile, product), deleting what it
/// downloaded.
///
/// A no-op when nothing is running for that pair — safe to call
/// defensively. A download that is already paused, with nothing running,
/// is deleted with [`super::discard`] instead.
pub async fn cancel(account_id: &str, product_id: &str) {
    let key: InstallKey = (account_id.to_string(), product_id.to_string());
    // Set unconditionally, not just when `active()` came up empty: a
    // download-phase cancel below already stops the install on its own,
    // but recording it here too costs nothing and means the early check in
    // `install` doesn't need to know which phase a given cancel landed in.
    stop_requested().lock().await.insert(key.clone(), Stop::Cancel);
    let in_flight = active().lock().await.remove(&key);
    let was_active = in_flight.is_some();
    if let Some(in_flight) = in_flight {
        in_flight.cancel();
    }
    tracing::debug!(account_id, product_id, was_active, "install cancel requested");
}

/// Pauses an install for this (profile, product), queued or downloading.
///
/// It stops within one chunk, records what it had synced and fails with
/// [`super::Paused`], keeping its data for [`super::resume`]. A cancel
/// already asked for stays a cancel.
pub async fn pause(account_id: &str, product_id: &str) {
    let key: InstallKey = (account_id.to_string(), product_id.to_string());
    stop_requested().lock().await.entry(key.clone()).or_insert(Stop::Pause);
    let in_flight = active().lock().await.get(&key).cloned();
    let was_active = in_flight.is_some();
    if let Some(in_flight) = in_flight {
        in_flight.pause();
    }
    tracing::debug!(account_id, product_id, was_active, "install pause requested");
}
