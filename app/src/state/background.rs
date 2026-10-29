//! State written by the long-running background jobs — installs and the
//! Play/Stop watch loop. Owned by the app, not by whichever view rendered
//! the button that started the job, so the job outlives that view.

use std::collections::{HashMap, HashSet};

use decanter_ps::process::Job;
use gpui::{Context, Entity};

use crate::i18n::Text;

/// One download per product and account: the same game owned on two
/// accounts downloads into two staging folders, so it has two rows too.
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct DownloadKey {
    pub account_id: String,
    pub product_id: String,
}

impl DownloadKey {
    pub fn new(account_id: &str, product_id: &str) -> Self {
        Self {
            account_id: account_id.to_owned(),
            product_id: product_id.to_owned(),
        }
    }

    pub fn is(&self, account_id: &str, product_id: &str) -> bool {
        self.account_id == account_id && self.product_id == product_id
    }

    /// For element ids, which want one string.
    pub fn id(&self) -> String {
        format!("{}-{}", self.account_id, self.product_id)
    }
}

/// What a download is for, so Descargas can tell an update from a first
/// install of the same game.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum DownloadKind {
    Install,
    Dlc,
    Update,
    BranchSwitch,
    Rollback,
    Repair,
}

#[derive(Clone, PartialEq)]
pub struct Download {
    pub title: String,
    pub kind: DownloadKind,
    /// Which run owns the row, so a run that ends after a newer one of the
    /// same key started (which stops it) leaves the newer one's row alone.
    pub run: u64,
    pub status: InstallStatus,
}

#[derive(Clone, PartialEq)]
pub enum InstallStatus {
    /// Enqueued, but no bytes have moved yet — the depot manifest is
    /// still being resolved, or other installs are ahead of this one in
    /// the download manager's queue.
    Queued,
    Installing {
        current_file: String,
        bytes_downloaded: u64,
        total_bytes: Option<u64>,
        bytes_per_second: f64,
    },
    /// Asked to stop; the row goes once the transfer has stopped and its
    /// staging is gone. Keeps what had landed for the user to see meanwhile.
    Cancelling {
        bytes_downloaded: u64,
    },
    /// Stopped with its data kept in the library's `downloading/`; a later
    /// install of it carries on from `bytes_done`.
    Paused {
        reason: decanter_gog::PauseReason,
        bytes_done: u64,
        total_bytes: Option<u64>,
    },
    Failed(Text),
}

impl InstallStatus {
    /// `0.0`–`1.0`, or `None` while the total is still unknown.
    pub fn fraction(&self) -> Option<f64> {
        match self {
            InstallStatus::Installing {
                bytes_downloaded,
                total_bytes: Some(total),
                ..
            }
            | InstallStatus::Paused {
                bytes_done: bytes_downloaded,
                total_bytes: Some(total),
                ..
            } if *total > 0 => Some((*bytes_downloaded as f64 / *total as f64).clamp(0.0, 1.0)),
            _ => None,
        }
    }

    /// What a cancel would delete, as far as the progress reported knows.
    pub const fn bytes_downloaded(&self) -> u64 {
        match self {
            InstallStatus::Installing { bytes_downloaded, .. }
            | InstallStatus::Cancelling { bytes_downloaded }
            | InstallStatus::Paused {
                bytes_done: bytes_downloaded,
                ..
            } => *bytes_downloaded,
            InstallStatus::Queued | InstallStatus::Failed(_) => 0,
        }
    }

    /// Queued, downloading or stopping. A failure has settled back to what
    /// was there before, and a paused download moves no bytes, so another
    /// install of it can start.
    pub const fn is_live(&self) -> bool {
        !matches!(self, InstallStatus::Failed(_) | InstallStatus::Paused { .. })
    }
}

#[derive(Default)]
pub struct Background {
    /// Game ids currently running — Library's Play/Stop reads it, and the
    /// external games window locks a running game's Eliminar with it. By game id, not prefix: a prefix belongs to exactly one
    /// (profile, game).
    pub running: HashSet<String>,
    /// The latest run of each game, finished ones included, for its log.
    pub runs: HashMap<String, Entity<Job>>,
    /// Every GOG download in flight (installs, DLC, updates, branch and
    /// build switches, repairs) and the failures not yet dismissed.
    pub installing: HashMap<DownloadKey, Download>,
    /// The download whose Cancelar was pressed and now waits for the user
    /// to confirm deleting what it downloaded (#1133). One at a time, and
    /// shared, so Descargas and the game's own page ask the same question.
    pub cancel_confirm: Option<DownloadKey>,
    /// Bumped after a play session's duration is written to the playtime
    /// table, so the detail pane's stat re-reads it.
    pub playtime_generation: u32,
    /// Bumped whenever favourites or collections change. Both the sidebar
    /// shelves and the cover browser group by them, and either can be
    /// edited from the other's context menu or from the detail pane — so
    /// one shared counter, not one per view, or a star set in one place
    /// leaves the others showing stale shelves.
    pub organisation: u32,
}

impl Background {
    pub fn status(&self, account_id: &str, product_id: &str) -> Option<&InstallStatus> {
        self.installing
            .iter()
            .find(|(key, _)| key.is(account_id, product_id))
            .map(|(_, download)| &download.status)
    }

    /// Nudges every view that groups by favourites or collections into
    /// re-reading them.
    pub fn bump_organisation(&mut self, cx: &mut Context<Self>) {
        self.organisation = self.organisation.wrapping_add(1);
        cx.notify();
    }
}
