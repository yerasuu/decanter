//! Process-wide install bookkeeping: which downloads are in flight, which
//! (profile, game) pairs have been asked to stop — shared state [`cancel`]
//! and [`pause`] need regardless of which phase an install is in — and whose
//! turn it is to download.

use core::future::Future;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use decanter_core::Library;
use decanter_runtime::journal::{LockError, StagingLock};
use tokio::sync::{Mutex, Notify, Semaphore, SemaphorePermit};

use super::download::Cancel;
use super::staging::DOWNLOADING;

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
    /// decanter is quitting; kept, and resumed on its own at the next start.
    Quit,
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

/// One download runs at a time (owner's decision, #1281): the rest wait for
/// their turn, in the order they asked, since the semaphore is fair.
static TURN: Semaphore = Semaphore::const_new(1);

/// Wakes the downloads waiting for their turn so they see a stop asked of them.
static STOPS: Notify = Notify::const_new();

/// How often a download waiting on another decanter process looks at the slot
/// again: file locks wake no one when they are let go.
const SLOT_POLL: Duration = Duration::from_secs(1);

/// The turn to download, held until dropped: this process's own and the slot
/// shared with every other decanter process using the library.
pub(super) struct Turn {
    // Let go of before the permit, so this process's next download finds it free.
    _slot: Option<StagingLock>,
    _permit: SemaphorePermit<'static>,
}

/// Runs `work` until it ends, or until a stop for `key` (or decanter quitting)
/// comes first, which drops it and returns `None`; the stop stays recorded for
/// the caller to honour.
pub(super) async fn until_stopped<T>(key: &InstallKey, work: impl Future<Output = T>) -> Option<T> {
    // Pinned once, outside the loop, so waking up for someone else's stop
    // doesn't cost this download its place in the queue.
    tokio::pin!(work);
    loop {
        let stopped = STOPS.notified();
        tokio::pin!(stopped);
        // Registered before the check, so a stop landing in between still wakes this.
        stopped.as_mut().enable();
        if is_quitting() || stop_requested().lock().await.contains_key(key) {
            return None;
        }
        tokio::select! {
            done = &mut work => return Some(done),
            () = &mut stopped => {}
        }
    }
}

/// Waits for `key`'s turn to download: first this process's, then the slot
/// other decanter processes share (#1320), so the CLI and the app never
/// download at once. `None` when a stop comes first, as [`until_stopped`].
pub(super) async fn wait_turn(library: &Library, key: &InstallKey) -> Option<Turn> {
    let Some(acquired) = until_stopped(key, TURN.acquire()).await else {
        return None;
    };
    let permit = match acquired {
        Ok(permit) => permit,
        Err(err) => {
            tracing::error!(error = %err, "the download queue is closed");
            return None;
        }
    };
    let Some(slot) = until_stopped(key, take_slot(library)).await else {
        return None;
    };
    Some(Turn {
        _slot: slot,
        _permit: permit,
    })
}

/// The folder whose lock is the slot: the library folder every process using
/// `library` agrees on whichever one it targets, the first present in order.
fn slot_dir(library: &Library) -> Option<PathBuf> {
    library
        .roots()
        .into_iter()
        .filter(|root| root.is_dir())
        .min()
        .map(|root| root.join(DOWNLOADING))
}

/// `None` when the slot can't be locked at all (a read-only disk, say): the
/// download then goes on without it rather than never.
async fn take_slot(library: &Library) -> Option<StagingLock> {
    let Some(dir) = slot_dir(library) else {
        return None;
    };
    if let Err(err) = tokio::fs::create_dir_all(&dir).await {
        tracing::warn!(dir = %dir.display(), error = %err, "couldn't create the shared download slot, downloading without it");
        return None;
    }
    let mut is_waiting = false;
    loop {
        match decanter_runtime::journal::lock(&dir) {
            Ok(slot) => {
                if is_waiting {
                    tracing::info!(dir = %dir.display(), "the other decanter process's download ended, taking the slot");
                }
                return Some(slot);
            }
            Err(LockError::Busy(_)) => {
                if !is_waiting {
                    tracing::info!(dir = %dir.display(), "another decanter process is downloading, waiting for its turn");
                    is_waiting = true;
                }
                tokio::time::sleep(SLOT_POLL).await;
            }
            Err(err) => {
                tracing::warn!(error = %err, "couldn't lock the shared download slot, downloading without it");
                return None;
            }
        }
    }
}

/// Whether another decanter process holds `library`'s download slot right now,
/// so a download started here would wait for it.
#[must_use]
#[inline]
pub fn download_slot_busy(library: &Library) -> bool {
    slot_dir(library).is_some_and(|dir| matches!(decanter_runtime::journal::lock(&dir), Err(LockError::Busy(_))))
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
    STOPS.notify_waiters();
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
/// [`super::Paused`], keeping its data for [`super::resume`]. One still
/// waiting or being planned stops at once, without asking GOG for anything,
/// and records its request to be planned again. A cancel already asked for
/// stays a cancel.
pub async fn pause(account_id: &str, product_id: &str) {
    let key: InstallKey = (account_id.to_string(), product_id.to_string());
    stop_requested().lock().await.entry(key.clone()).or_insert(Stop::Pause);
    STOPS.notify_waiters();
    let in_flight = active().lock().await.get(&key).cloned();
    let was_active = in_flight.is_some();
    if let Some(in_flight) = in_flight {
        in_flight.pause();
    }
    tracing::debug!(account_id, product_id, was_active, "install pause requested");
}

/// Downloads between taking their stop flag and recording how they ended.
///
/// A quit waits on this rather than `active()`, which lets go of a download
/// before its journal records the pause.
static SETTLING: AtomicUsize = AtomicUsize::new(0);

static QUITTING: AtomicBool = AtomicBool::new(false);

/// decanter is quitting: a download starting now is paused before any byte moves.
pub(super) fn is_quitting() -> bool {
    QUITTING.load(Ordering::Acquire)
}

/// Held by a download from taking its stop flag until its journal records how it ended.
pub(super) struct Settling(());

impl Settling {
    /// Counted until dropped.
    pub(super) fn start() -> Self {
        SETTLING.fetch_add(1, Ordering::AcqRel);
        Self(())
    }
}

impl Drop for Settling {
    fn drop(&mut self) {
        SETTLING.fetch_sub(1, Ordering::AcqRel);
    }
}

/// Pauses every download this process runs because decanter is quitting.
///
/// Waits up to `wait` for their journals to record it, and returns whether
/// all of them did; one that didn't is resumed as after a crash, which is safe.
pub async fn pause_for_quit(wait: Duration) -> bool {
    // Set before the running ones are collected, so a download whose plan
    // lands meanwhile stops as soon as it takes its flag.
    QUITTING.store(true, Ordering::Release);
    STOPS.notify_waiters();
    let running: Vec<Cancel> = active().lock().await.values().cloned().collect();
    for in_flight in &running {
        in_flight.quit();
    }
    let deadline = Instant::now() + wait;
    while SETTLING.load(Ordering::Acquire) > 0 {
        if Instant::now() >= deadline {
            tracing::warn!(
                left = SETTLING.load(Ordering::Acquire),
                "downloads still stopping at quit; they resume as after a crash"
            );
            return false;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    tracing::info!(paused = running.len(), "downloads paused for quit");
    true
}
