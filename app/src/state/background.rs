//! State written by the long-running background jobs — installs and the
//! Play/Stop watch loop. Owned by the app, not by whichever view rendered
//! the button that started the job, so the job outlives that view.

use std::collections::{HashMap, HashSet};

use decanter_ps::process::Job;
use gpui::{Context, Entity};

use crate::i18n::Text;

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
            } if *total > 0 => Some((*bytes_downloaded as f64 / *total as f64).clamp(0.0, 1.0)),
            _ => None,
        }
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
    /// Installs in progress, keyed by GOG product id.
    pub installing: HashMap<String, InstallStatus>,
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
    /// Nudges every view that groups by favourites or collections into
    /// re-reading them.
    pub fn bump_organisation(&mut self, cx: &mut Context<Self>) {
        self.organisation = self.organisation.wrapping_add(1);
        cx.notify();
    }
}
