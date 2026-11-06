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

fn active() -> &'static Mutex<HashMap<Key, StopFlag>> {
    static ACTIVE: OnceLock<Mutex<HashMap<Key, StopFlag>>> = OnceLock::new();
    ACTIVE.get_or_init(Mutex::default)
}

/// A map of flags stays consistent whatever panicked while holding it, so a poisoned lock is used.
fn lock_active() -> std::sync::MutexGuard<'static, HashMap<Key, StopFlag>> {
    active().lock().unwrap_or_else(PoisonError::into_inner)
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
        let mut active = lock_active();
        if active
            .get(&self.key)
            .is_some_and(|flag| Arc::ptr_eq(&flag.0, &self.flag.0))
        {
            active.remove(&self.key);
        }
    }
}

/// Registers a fresh run of `(account_id, product_id)`. One already running for it is paused,
/// since two runs would write the same staging; the new one carries on from what it kept.
pub(super) fn register(account_id: &str, product_id: &str) -> Registration {
    let key = (account_id.to_owned(), product_id.to_owned());
    let flag = StopFlag::default();
    if QUITTING.load(Ordering::Acquire) {
        flag.set(QUIT);
    }
    let previous = lock_active().insert(key.clone(), flag.clone());
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
/// Nothing happens when none runs: a paused one is deleted with [`super::discard`].
#[inline]
pub fn cancel(account_id: &str, product_id: &str) {
    let flag = lock_active()
        .get(&(account_id.to_owned(), product_id.to_owned()))
        .cloned();
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
    let flag = lock_active()
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
    let running: Vec<StopFlag> = lock_active().values().cloned().collect();
    for flag in &running {
        flag.set(QUIT);
    }
    let deadline = Instant::now() + wait;
    loop {
        let left = lock_active().len();
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
