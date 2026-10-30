//! The order downloads take their one turn in (#1349): an explicit list the user can reorder,
//! instead of the arrival order a fair semaphore gives.
//!
//! Every download that has a place is in `order`: the one running, the ones waiting, and the ones
//! stopped meanwhile, which keep their place so a resume (or the next start, after a quit or a
//! crash) carries on from it. A place is given up only once the download is over: finished,
//! cancelled, failed or discarded. The app persists the order in `<root>/downloading/queue.json`
//! once it has restored it; the CLI never does, so its own queue stays in arrival order and never
//! overwrites the app's.

use std::collections::HashSet;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, OnceLock, PoisonError};
use std::time::{Duration, Instant};

use decanter_core::Library;
use serde::{Deserialize, Serialize};
use tokio::sync::Notify;

use super::tracking::InstallKey;

const QUEUE_FILE: &str = "queue.json";
const FORMAT: u32 = 1;

/// How long the downloads the startup scan resumes are waited for before one behind them in the
/// saved order may start: each reaches the queue on its own task, in whatever order they run.
const ARRIVAL_GRACE: Duration = Duration::from_secs(5);

/// Where a queued download is moved to, among the downloads waiting for their turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum QueueMove {
    Up,
    Down,
    /// Next to start, once the one running ends; it is never interrupted.
    First,
    Last,
    /// That position, from 0, clamped to the queue.
    To(usize),
}

/// The queue as it stands: the download holding the turn, and the ones waiting for it in the
/// order they will start. Each as (account, product).
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct DownloadQueue {
    pub running: Option<(String, String)>,
    pub waiting: Vec<(String, String)>,
}

impl DownloadQueue {
    /// Where (account, product) waits, from 0 for the next to start.
    #[must_use]
    #[inline]
    pub fn position(&self, account_id: &str, product_id: &str) -> Option<usize> {
        self.waiting
            .iter()
            .position(|(account, product)| account == account_id && product == product_id)
    }

    #[must_use]
    #[inline]
    pub fn is_running(&self, account_id: &str, product_id: &str) -> bool {
        self.running
            .as_ref()
            .is_some_and(|(account, product)| account == account_id && product == product_id)
    }
}

#[derive(Default)]
struct Queue {
    order: Vec<InstallKey>,
    /// Tickets of the downloads in [`take_turn`], by arrival: the same product can wait twice
    /// while a newer run of it pauses the older.
    waiting: Vec<(u64, InstallKey)>,
    running: Option<InstallKey>,
    /// Restored places of downloads the startup scan resumes and that haven't joined yet.
    expected: HashSet<InstallKey>,
    expected_until: Option<Instant>,
    file: Option<PathBuf>,
    loaded: HashSet<PathBuf>,
    next_ticket: u64,
}

fn queue() -> MutexGuard<'static, Queue> {
    static QUEUE: OnceLock<Mutex<Queue>> = OnceLock::new();
    // Every change below leaves the queue consistent at each step, so one a panic cut short is
    // still safe to read.
    QUEUE
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
}

/// Rung on every change of the turn or the order, for the waiters and the app's page.
static CHANGED: Notify = Notify::const_new();

/// Serialises writes of the file, so the last one to land is the newest order.
static WRITING: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

impl Queue {
    fn rank(&self, key: &InstallKey) -> usize {
        self.order.iter().position(|placed| placed == key).unwrap_or(usize::MAX)
    }

    fn is_waiting(&self, key: &InstallKey) -> bool {
        self.waiting.iter().any(|(_, waiting)| waiting == key)
    }

    fn queued(&self) -> Vec<InstallKey> {
        let mut queued: Vec<InstallKey> = Vec::with_capacity(self.waiting.len());
        let mut by_rank: Vec<&(u64, InstallKey)> = self.waiting.iter().collect();
        by_rank.sort_by_key(|(ticket, key)| (self.rank(key), *ticket));
        for (_, key) in by_rank {
            if !queued.contains(key) && self.running.as_ref() != Some(key) {
                queued.push(key.clone());
            }
        }
        queued
    }

    fn join(&mut self, key: &InstallKey) -> (u64, bool) {
        let ticket = self.next_ticket;
        self.next_ticket = self.next_ticket.wrapping_add(1);
        self.waiting.push((ticket, key.clone()));
        self.expected.remove(key);
        let is_new = !self.order.contains(key);
        if is_new {
            self.order.push(key.clone());
        }
        (ticket, is_new)
    }

    /// Takes the turn for `ticket` when it is free and `ticket` is first; otherwise when to look
    /// again on its own, if a restored place ahead of it stops being waited for then.
    fn try_take(&mut self, ticket: u64, now: Instant) -> Result<(), Option<Instant>> {
        if self.running.is_some() {
            return Err(None);
        }
        let head = self
            .waiting
            .iter()
            .min_by_key(|(waiting, key)| (self.rank(key), *waiting))
            .map(|(waiting, key)| (*waiting, key.clone()));
        let Some((head, key)) = head.filter(|(head, _)| *head == ticket) else {
            return Err(None);
        };
        if let Some(until) = self.expected_until {
            if now < until {
                let rank = self.rank(&key);
                if self.expected.iter().any(|expected| self.rank(expected) < rank) {
                    return Err(Some(until));
                }
            } else {
                self.expected.clear();
                self.expected_until = None;
            }
        }
        self.waiting.retain(|(waiting, _)| *waiting != head);
        // First from now on, so a quit or a crash brings it back ahead of every other.
        self.order.retain(|placed| *placed != key);
        self.order.insert(0, key.clone());
        self.running = Some(key);
        Ok(())
    }

    fn move_queued(&mut self, key: &InstallKey, to: QueueMove) -> bool {
        let mut queued = self.queued();
        let Some(from) = queued.iter().position(|queued| queued == key) else {
            return false;
        };
        let last = queued.len().saturating_sub(1);
        let target = match to {
            QueueMove::Up => from.saturating_sub(1),
            QueueMove::Down => from.saturating_add(1).min(last),
            QueueMove::First => 0,
            QueueMove::Last => last,
            QueueMove::To(index) => index.min(last),
        };
        if target == from {
            return false;
        }
        let moved = queued.remove(from);
        queued.insert(target, moved);
        // The waiting downloads trade the places they hold among themselves; the running one and
        // the stopped ones keep theirs.
        let mut reordered = queued.into_iter();
        let waiting: HashSet<InstallKey> = self.waiting.iter().map(|(_, key)| key.clone()).collect();
        for placed in &mut self.order {
            if waiting.contains(placed)
                && self.running.as_ref() != Some(placed)
                && let Some(next) = reordered.next()
            {
                *placed = next;
            }
        }
        true
    }

    fn forget(&mut self, key: &InstallKey) -> bool {
        if self.is_waiting(key) || self.running.as_ref() == Some(key) {
            return false;
        }
        let before = self.order.len();
        self.order.retain(|placed| placed != key);
        self.order.len() != before
    }
}

/// A download's place while it waits, and its turn once it has it; let go of when dropped.
pub(super) struct Place {
    ticket: u64,
    is_held: bool,
}

impl Drop for Place {
    fn drop(&mut self) {
        let mut queue = queue();
        if self.is_held {
            queue.running = None;
        } else {
            let ticket = self.ticket;
            queue.waiting.retain(|(waiting, _)| *waiting != ticket);
        }
        drop(queue);
        CHANGED.notify_waiters();
    }
}

/// Waits until `key` is first in the order and the turn is free, then holds it.
///
/// Dropping the future gives the place up, so a stop ends the wait at once.
pub(super) async fn take_turn(key: &InstallKey) -> Place {
    let (ticket, is_new) = queue().join(key);
    let mut place = Place { ticket, is_held: false };
    CHANGED.notify_waiters();
    if is_new {
        persist().await;
    }
    loop {
        let changed = CHANGED.notified();
        tokio::pin!(changed);
        // Registered before the check, so a turn let go of in between still wakes this.
        changed.as_mut().enable();
        let taken = queue().try_take(ticket, Instant::now());
        match taken {
            Ok(()) => {
                place.is_held = true;
                CHANGED.notify_waiters();
                persist().await;
                return place;
            }
            Err(Some(until)) => {
                tokio::select! {
                    () = &mut changed => {}
                    () = tokio::time::sleep_until(until.into()) => {}
                }
            }
            Err(None) => changed.await,
        }
    }
}

/// Gives up `key`'s place once its download is over; one only stopped keeps it.
pub(super) async fn settle(key: &InstallKey, outcome: &anyhow::Result<()>) {
    if outcome.as_ref().is_err_and(anyhow::Error::is::<super::Paused>) {
        return;
    }
    forget(key).await;
}

pub(super) async fn forget(key: &InstallKey) {
    let is_changed = queue().forget(key);
    if is_changed {
        CHANGED.notify_waiters();
        persist().await;
    }
}

/// The queue right now, for the Downloads page.
#[must_use]
#[inline]
pub fn download_queue() -> DownloadQueue {
    let queue = queue();
    DownloadQueue {
        running: queue.running.clone(),
        waiting: queue.queued(),
    }
}

/// Moves a waiting download within the queue. Returns whether it moved; one running, stopped or
/// not queued stays where it is.
#[inline]
pub async fn move_queued(account_id: &str, product_id: &str, to: QueueMove) -> bool {
    let key: InstallKey = (account_id.to_owned(), product_id.to_owned());
    let is_moved = queue().move_queued(&key, to);
    if is_moved {
        tracing::info!(account_id, product_id, ?to, "download moved in the queue");
        CHANGED.notify_waiters();
        persist().await;
    }
    is_moved
}

/// Resolves once the queue changes again.
#[inline]
pub async fn queue_changed() {
    CHANGED.notified().await;
}

/// Takes the order saved in `library`'s download folder and keeps it there from now on.
///
/// `listed` are the downloads its folders hold unfinished: a saved place of any other is dropped,
/// since nothing will ever claim it. `resuming` are those about to be resumed, which are waited
/// for briefly so one behind them doesn't take the turn first. A missing file is an empty order; an
/// unreadable one is logged and replaced, and the downloads start in arrival order.
#[inline]
pub async fn restore_queue(library: &Library, listed: &[(String, String)], resuming: &[(String, String)]) {
    let Some(dir) = super::tracking::slot_dir(library) else {
        return;
    };
    let path = dir.join(QUEUE_FILE);
    let is_first = !queue().loaded.contains(&dir);
    let saved = if is_first { load(&path).await } else { Vec::new() };
    {
        let mut queue = queue();
        queue.loaded.insert(dir);
        let restored = saved.len();
        for key in saved {
            if listed.contains(&key) && !queue.order.contains(&key) {
                queue.order.push(key);
            }
        }
        if is_first {
            tracing::info!(
                path = %path.display(),
                restored,
                kept = queue.order.len(),
                "download queue order restored"
            );
        }
        for key in resuming {
            if queue.order.contains(key) && !queue.is_waiting(key) && queue.running.as_ref() != Some(key) {
                queue.expected.insert(key.clone());
            }
        }
        if !queue.expected.is_empty() {
            queue.expected_until = Some(Instant::now() + ARRIVAL_GRACE);
        }
        queue.file = Some(path);
    }
    CHANGED.notify_waiters();
    persist().await;
}

#[derive(Serialize, Deserialize)]
struct QueueFile {
    format: u32,
    entries: Vec<Entry>,
}

#[derive(Serialize, Deserialize)]
struct Entry {
    account_id: String,
    product_id: String,
    seq: u64,
}

/// Ids as GOG gives them; anything else in the file is not a place this process handed out.
fn is_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
}

async fn load(path: &Path) -> Vec<InstallKey> {
    let bytes = match tokio::fs::read(path).await {
        Ok(bytes) => bytes,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Vec::new(),
        Err(err) => {
            tracing::warn!(path = %path.display(), error = %err, "couldn't read the saved download queue, using arrival order");
            return Vec::new();
        }
    };
    let Some(file) = parse(path, &bytes) else {
        return Vec::new();
    };
    let (order, skipped) = ordered(file.entries);
    if skipped > 0 {
        tracing::warn!(path = %path.display(), skipped, "skipped malformed download queue entries");
    }
    order
}

fn parse(path: &Path, bytes: &[u8]) -> Option<QueueFile> {
    match serde_json::from_slice::<QueueFile>(bytes) {
        Ok(file) if file.format == FORMAT => Some(file),
        Ok(file) => {
            tracing::warn!(path = %path.display(), format = file.format, "unknown download queue format, using arrival order");
            None
        }
        Err(err) => {
            tracing::warn!(path = %path.display(), error = %err, "the saved download queue is corrupt, using arrival order");
            None
        }
    }
}

/// The places `entries` give, in sequence, each product once; and how many entries were not ids.
fn ordered(mut entries: Vec<Entry>) -> (Vec<InstallKey>, usize) {
    entries.sort_by_key(|entry| entry.seq);
    let mut order: Vec<InstallKey> = Vec::with_capacity(entries.len());
    let mut skipped = 0_usize;
    for entry in entries {
        if !is_id(&entry.account_id) || !is_id(&entry.product_id) {
            skipped += 1;
            continue;
        }
        let key = (entry.account_id, entry.product_id);
        if !order.contains(&key) {
            order.push(key);
        }
    }
    (order, skipped)
}

/// Writes the order to the restored file, if any. A failure is logged and the order kept in
/// memory: the downloads lose nothing, only their places after a restart.
async fn persist() {
    let _writing = WRITING.lock().await;
    let (path, entries) = {
        let queue = queue();
        (queue.file.clone(), queue.order.clone())
    };
    let Some(path) = path else {
        return;
    };
    let entries = entries
        .into_iter()
        .zip(0..)
        .map(|((account_id, product_id), seq)| Entry {
            account_id,
            product_id,
            seq,
        })
        .collect();
    let file = QueueFile {
        format: FORMAT,
        entries,
    };
    let bytes = match serde_json::to_vec_pretty(&file) {
        Ok(bytes) => bytes,
        Err(err) => {
            tracing::warn!(error = %err, "couldn't encode the download queue");
            return;
        }
    };
    let written = async {
        if let Some(dir) = path.parent() {
            tokio::fs::create_dir_all(dir).await?;
        }
        decanter_runtime::journal::write_atomic(&path, &bytes).await
    }
    .await;
    if let Err(err) = written {
        tracing::warn!(path = %path.display(), error = %err, "couldn't save the download queue order");
    }
}
