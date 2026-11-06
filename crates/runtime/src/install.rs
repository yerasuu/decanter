//! What every store's install reports and stops with, so the app shows a GOG and an Amazon download
//! the same way, and the download slot they share so only one of them downloads at a time.

use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime};

use decanter_core::{Library, Storefront};
use serde::{Deserialize, Serialize};

use crate::journal::{LockError, StagingLock};

/// The folder under a library root that holds the shared slot's lock.
const SLOT_FOLDER: &str = "downloading";

/// How often a download waiting on another decanter process looks at the slot
/// again: file locks wake no one when they are let go.
const SLOT_POLL: Duration = Duration::from_secs(1);

/// Why a download stopped before finishing. Its data stays in staging until the user cancels it.
///
/// Exhaustive on purpose: the app words each reason, so a new one has to reach it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PauseReason {
    User,
    /// No answer from the store for the whole retry window, or a refused download URL, which the
    /// next plan asks for again.
    Network,
    /// decanter quit cleanly while it ran.
    AppClosed,
    /// decanter stopped without pausing it: a crash, a kill, a power cut.
    ClosedUnexpectedly,
    DiskFull,
    /// The library folder it downloads into went away (an unplugged disk).
    LibraryUnavailable,
}

/// The free space a download needed on its library's disk, against what that disk had, when it
/// stopped for it.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Room {
    /// What was left to download, plus a margin so the disk is never filled to the last byte.
    pub needed: u64,
    pub free: u64,
}

impl Room {
    #[must_use]
    #[inline]
    pub const fn new(needed: u64, free: u64) -> Self {
        Self { needed, free }
    }

    /// How much more has to be freed.
    #[must_use]
    #[inline]
    pub const fn short(&self) -> u64 {
        self.needed.saturating_sub(self.free)
    }
}

/// What a download fails with when it stopped and kept what it had: resuming it carries on from
/// `bytes_done`.
#[derive(Debug, thiserror::Error)]
#[error("download paused ({reason:?}) with {bytes_done} bytes kept")]
pub struct Paused {
    pub reason: PauseReason,
    pub bytes_done: u64,
    /// The library folder it downloads into, so the app can tell when an unplugged one is back.
    pub root: Option<PathBuf>,
    /// For a full disk, how much it needed.
    pub room: Option<Room>,
}

/// What a download stopped by the user's cancel fails with, so a caller can tell it from a failure
/// worth reporting.
#[derive(Debug, thiserror::Error)]
#[error("download cancelled")]
pub struct Cancelled;

/// A download a run left unfinished, as each store finds it in its own staging, so the app lists
/// a GOG and an Amazon one as the same paused row.
#[derive(Debug, Clone)]
pub struct UnfinishedDownload {
    /// Whose install records and resume it goes through.
    pub storefront: Storefront,
    pub account_id: String,
    pub product_id: String,
    /// The base game, for a DLC.
    pub parent_id: Option<String>,
    pub title: String,
    /// Carried on as a repair rather than an install.
    pub is_repair: bool,
    /// Another language's files added to a game already installed.
    pub is_language: bool,
    /// A pinned build (a rollback), which a resume keeps.
    pub build_id: Option<String>,
    /// The last pause its staging recorded, or [`PauseReason::ClosedUnexpectedly`] when it
    /// recorded none.
    pub reason: PauseReason,
    /// For a full disk, how much it needed.
    pub room: Option<Room>,
    pub bytes_done: u64,
    /// `None` for a download queued and never planned.
    pub total_bytes: Option<u64>,
    /// The library folder it downloads into.
    pub root: PathBuf,
    /// Another decanter process (the CLI, say) is running it right now.
    pub in_use: bool,
    /// When its staging last recorded anything, which for a paused download is when it paused.
    pub paused_since: Option<SystemTime>,
    /// A DLC ticked with its game and never planned, which takes the game's reason.
    pub is_queued_behind_game: bool,
}

/// What a download switched build kept: the store published another build while it was stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BuildSwitch {
    /// Bytes the new build shares with the earlier one, and so still has.
    pub kept_bytes: u64,
    /// Bytes the earlier build had downloaded.
    pub had_bytes: u64,
}

/// Progress for the install as a whole, not for one file: several files
/// transfer at once, so per-file numbers jump around and never add up to
/// something a progress bar can use. `bytes_downloaded` and
/// `bytes_per_second` are summed across every file in flight.
#[derive(Debug, Clone)]
pub struct Progress {
    /// Whichever file most recently reported — a liveness hint, not a
    /// position in the install.
    pub current_file: String,
    pub bytes_downloaded: u64,
    /// The install's full size, known upfront from the store's manifest.
    pub total_bytes: Option<u64>,
    pub bytes_per_second: f64,
    /// Set when the store published another build while the download was stopped, on every
    /// report of the resumed run.
    pub build_switch: Option<BuildSwitch>,
}

/// Smooths the install's overall transfer rate over wall-clock time.
///
/// Summing what each file reports on its own spiked to several GB/s: a small file finishing
/// before its first normal sample divides its whole size by a sliver of time, and with no average
/// yet to soften it the spike becomes the rate outright. Measuring the install's total bytes here
/// avoids the per-file arithmetic that produces it.
pub struct RateTracker {
    last_sample_at: Instant,
    last_sample_bytes: u64,
    ema_bps: f64,
}

impl RateTracker {
    const EMA_ALPHA: f64 = 0.2;
    const MIN_SAMPLE_INTERVAL: Duration = Duration::from_millis(200);

    /// `on_disk` is what a resumed download already had, so it doesn't
    /// read as transferred in the first sample.
    #[must_use]
    #[inline]
    pub fn new(on_disk: u64) -> Self {
        Self {
            last_sample_at: Instant::now(),
            last_sample_bytes: on_disk,
            ema_bps: 0.0,
        }
    }

    /// Feed the install's total bytes downloaded so far. Returns the
    /// smoothed rate to report — unchanged from the last sample when too
    /// little time has passed to measure one without the same kind of
    /// tiny-denominator spike this exists to avoid.
    #[inline]
    pub fn sample(&mut self, total_bytes: u64) -> f64 {
        let now = Instant::now();
        let dt = now.duration_since(self.last_sample_at);
        if dt < Self::MIN_SAMPLE_INTERVAL {
            return self.ema_bps;
        }

        let byte_delta = total_bytes.saturating_sub(self.last_sample_bytes);
        self.last_sample_at = now;
        self.last_sample_bytes = total_bytes;

        let instantaneous = byte_delta as f64 / dt.as_secs_f64();
        self.ema_bps = if self.ema_bps <= 0.0 {
            instantaneous
        } else {
            Self::EMA_ALPHA * instantaneous + (1.0 - Self::EMA_ALPHA) * self.ema_bps
        };
        // `trace!`, not `debug!`: this samples on every progress tick, the
        // same per-chunk frequency as the download loop feeding it.
        tracing::trace!(
            byte_delta,
            dt_ms = dt.as_millis(),
            ema_bps = self.ema_bps,
            "sampled transfer rate"
        );
        self.ema_bps
    }
}

/// The folder whose lock is the download slot: the library folder every process using `library`
/// agrees on whichever one it targets, the first present in order.
#[must_use]
#[inline]
pub fn slot_dir(library: &Library) -> Option<PathBuf> {
    library
        .roots()
        .into_iter()
        .filter(|root| root.is_dir())
        .min()
        .map(|root| root.join(SLOT_FOLDER))
}

/// Waits for the download slot every store's downloads and every decanter process share (#1320),
/// so two downloads never run at once. `None` when the slot can't be locked at all (a read-only
/// disk, say): the download then goes on without it rather than never.
#[inline]
pub async fn take_slot(library: &Library) -> Option<StagingLock> {
    let Some(dir) = slot_dir(library) else {
        return None;
    };
    if let Err(err) = tokio::fs::create_dir_all(&dir).await {
        tracing::warn!(dir = %dir.display(), error = %err, "couldn't create the shared download slot, downloading without it");
        return None;
    }
    let mut is_waiting = false;
    loop {
        match crate::journal::lock(&dir) {
            Ok(slot) => {
                if is_waiting {
                    tracing::info!(dir = %dir.display(), "the other download ended, taking the slot");
                }
                return Some(slot);
            }
            Err(LockError::Busy(_)) => {
                if !is_waiting {
                    tracing::info!(dir = %dir.display(), "another download holds the slot, waiting for its turn");
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
