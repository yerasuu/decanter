// SPDX-License-Identifier: GPL-3.0-or-later
//! The Amazon downloads this process runs and how each was asked to stop, so the app's Pausar,
//! Cancelar and quit reach them by (account, product) the way they reach GOG's.

use core::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use core::time::Duration;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::time::Instant;

use tokio::sync::Notify;

type Key = (String, String);

const RUNNING: u8 = 0;
const CANCEL: u8 = 1;
const PAUSE: u8 = 2;
const QUIT: u8 = 3;
const ABORT: u8 = 4;

/// What stopped a download before it finished.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Stop {
    /// Confirmed by the user: everything it downloaded is deleted.
    Cancel,
    /// Kept on disk, to carry on from later.
    Pause,
    /// decanter is quitting; kept like a pause.
    Quit,
    /// Another file failed, and that failure is what the download ends with.
    Abort,
}

#[derive(Default)]
struct State {
    stop: AtomicU8,
    woken: Notify,
}

/// One download's stop request, checked between reads and awaited beside every network call so a
/// stop lands within one read rather than at the end of a file.
#[derive(Clone, Default)]
pub(super) struct StopFlag(Arc<State>);

impl StopFlag {
    /// A cancel replaces whatever else stopped the download, since it is the one that deletes it;
    /// the others only stop a running one.
    fn set(&self, stop: u8) {
        if stop == CANCEL {
            self.0.stop.store(CANCEL, Ordering::Release);
        } else {
            let _ = self
                .0
                .stop
                .compare_exchange(RUNNING, stop, Ordering::AcqRel, Ordering::Acquire);
        }
        self.0.woken.notify_waiters();
    }

    pub(super) fn abort(&self) {
        self.set(ABORT);
    }

    pub(super) fn requested(&self) -> Option<Stop> {
        match self.0.stop.load(Ordering::Acquire) {
            CANCEL => Some(Stop::Cancel),
            PAUSE => Some(Stop::Pause),
            QUIT => Some(Stop::Quit),
            ABORT => Some(Stop::Abort),
            _ => None,
        }
    }

    /// Resolves once any stop is requested.
    pub(super) async fn stopped(&self) {
        loop {
            let woken = self.0.woken.notified();
            tokio::pin!(woken);
            // Registered before the check, so a stop landing in between still wakes this.
            woken.as_mut().enable();
            if self.requested().is_some() {
                return;
            }
            woken.await;
        }
    }
}

/// How long a cancel that found no download running waits for one to register. The app shows
/// Cancelar as soon as it queues a download, before the download's task has run far enough to
/// register; past this, the cancel was for a download that had already ended, and must not land on
/// the next one.
const CANCEL_WAITS: Duration = Duration::from_secs(10);

#[derive(Default)]
struct Registry {
    running: HashMap<Key, StopFlag>,
    /// Cancels that came before their download registered, and when.
    cancelled: HashMap<Key, Instant>,
}

/// One lock for both maps, so a cancel and the registration it races can't both miss each other.
fn registry() -> &'static Mutex<Registry> {
    static REGISTRY: OnceLock<Mutex<Registry>> = OnceLock::new();
    REGISTRY.get_or_init(Mutex::default)
}

/// The maps stay consistent whatever panicked while holding them, so a poisoned lock is used.
fn lock_registry() -> std::sync::MutexGuard<'static, Registry> {
    registry().lock().unwrap_or_else(PoisonError::into_inner)
}

static QUITTING: AtomicBool = AtomicBool::new(false);

/// A download's place in the registry, given up when dropped, once its staging records how it
/// ended: a quit waits for that.
pub(super) struct Registration {
    key: Key,
    pub(super) flag: StopFlag,
}

impl Drop for Registration {
    fn drop(&mut self) {
        let mut registry = lock_registry();
        if registry
            .running
            .get(&self.key)
            .is_some_and(|flag| Arc::ptr_eq(&flag.0, &self.flag.0))
        {
            registry.running.remove(&self.key);
        }
    }
}

/// Registers a fresh run of `(account_id, product_id)`. One already running for it is paused,
/// since two runs would write the same staging; the new one carries on from what it kept. A cancel
/// that came before it registered stops it as soon as it checks its flag.
pub(super) fn register(account_id: &str, product_id: &str) -> Registration {
    let key = (account_id.to_owned(), product_id.to_owned());
    let flag = StopFlag::default();
    if QUITTING.load(Ordering::Acquire) {
        flag.set(QUIT);
    }
    let (previous, cancelled_at) = {
        let mut registry = lock_registry();
        let cancelled_at = registry.cancelled.remove(&key);
        (registry.running.insert(key.clone(), flag.clone()), cancelled_at)
    };
    match cancelled_at.map(|at| at.elapsed()) {
        Some(waited) if waited <= CANCEL_WAITS => {
            tracing::info!(
                account_id,
                product_id,
                waited_ms = waited.as_millis(),
                "an amazon download cancelled before it registered, stopping it"
            );
            flag.set(CANCEL);
        }
        Some(waited) => tracing::info!(
            account_id,
            product_id,
            waited_ms = waited.as_millis(),
            "an earlier cancel of this amazon download came too long ago, ignoring it"
        ),
        None => {}
    }
    if let Some(previous) = previous {
        tracing::info!(
            account_id,
            product_id,
            "an amazon download of this game was running, pausing it"
        );
        previous.set(PAUSE);
    }
    Registration { key, flag }
}

/// Cancels the Amazon download of `(account_id, product_id)`, which then deletes what it fetched.
///
/// One that has not registered yet is cancelled when it does, within [`CANCEL_WAITS`]; a paused one,
/// which nothing runs, is deleted with [`super::discard`].
#[inline]
pub fn cancel(account_id: &str, product_id: &str) {
    let key = (account_id.to_owned(), product_id.to_owned());
    let flag = {
        let mut registry = lock_registry();
        let now = Instant::now();
        registry
            .cancelled
            .retain(|_, at| now.duration_since(*at) <= CANCEL_WAITS);
        let flag = registry.running.get(&key).cloned();
        if flag.is_none() {
            registry.cancelled.insert(key, now);
        }
        flag
    };
    tracing::debug!(
        account_id,
        product_id,
        was_active = flag.is_some(),
        "amazon download cancel requested"
    );
    if let Some(flag) = flag {
        flag.set(CANCEL);
    }
}

/// Pauses the Amazon download of `(account_id, product_id)`, keeping what it fetched for the next
/// install of the game to carry on from. Nothing happens when none runs.
#[inline]
pub fn pause(account_id: &str, product_id: &str) {
    let flag = lock_registry()
        .running
        .get(&(account_id.to_owned(), product_id.to_owned()))
        .cloned();
    tracing::debug!(
        account_id,
        product_id,
        was_active = flag.is_some(),
        "amazon download pause requested"
    );
    if let Some(flag) = flag {
        flag.set(PAUSE);
    }
}

/// Pauses every Amazon download of this process because decanter is quitting, and waits up to
/// `wait` for each to record it. Returns whether all of them did; one that didn't carries on from
/// its last finished file next time, which is safe.
#[inline]
pub async fn pause_for_quit(wait: Duration) -> bool {
    QUITTING.store(true, Ordering::Release);
    let running: Vec<StopFlag> = lock_registry().running.values().cloned().collect();
    for flag in &running {
        flag.set(QUIT);
    }
    let deadline = Instant::now() + wait;
    loop {
        let left = lock_registry().running.len();
        if left == 0 {
            tracing::info!(paused = running.len(), "amazon downloads paused for quit");
            return true;
        }
        if Instant::now() >= deadline {
            tracing::warn!(
                left,
                "amazon downloads still stopping at quit; they resume from their last file"
            );
            return false;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}
