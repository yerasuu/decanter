//! State written from `spawn_forever`'d tasks — installs and the Play/Stop
//! watch loop, the two genuinely long-running background jobs that must
//! survive whatever UI restructuring their own progress causes. Dioxus
//! ties a `spawn_forever`'d task to the *root* scope, so anything it
//! writes to has to be owned there too — not by whichever routed view
//! happens to render the button that started it. Confirmed by a real
//! runtime warning: writing a `Library`-scoped `Signal` from a
//! root-scoped task, which silently drops the value out from under the
//! task the moment `Library` itself unmounts.

use std::collections::{HashMap, HashSet};

use dioxus::core::ScopeId;
use dioxus::prelude::*;

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
    Failed(String),
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

#[derive(Clone, Copy)]
pub struct Background {
    /// Product ids currently running — shared between Library's Play/Stop
    /// and the compatibility screen's, so launching from either is
    /// reflected in both.
    pub running: crate::launch::Running,
    /// Installs in progress, keyed by GOG product id.
    pub installing: Signal<HashMap<String, InstallStatus>>,
    /// Bumped after a play session's duration is written to the playtime
    /// table, so the detail pane's stat re-reads it.
    pub playtime_generation: Signal<u32>,
    /// Bumped whenever favourites or collections change. Both the sidebar
    /// shelves and the cover browser group by them, and either can be
    /// edited from the other's context menu or from the detail pane — so
    /// one shared counter, not one per view, or a star set in one place
    /// leaves the others showing stale shelves.
    pub organisation: Signal<u32>,
}

/// Nudges every view that groups by favourites or collections into
/// re-reading them.
pub fn bump_organisation(mut generation: Signal<u32>) {
    let next = generation.peek().wrapping_add(1);
    generation.set(next);
}

/// Call once, at the app root. `use_signal` alone doesn't guarantee its
/// storage lands in `ScopeId::ROOT` just because this happens to be
/// called from `App` — confirmed by a real runtime warning (two
/// *different* scopes, neither the app's own, showed up as "owner" and
/// "used from" for the exact same call site). `Signal::new_in_scope`
/// pins the storage there explicitly, since `spawn_forever` always
/// resolves to that literal constant regardless of which scope actually
/// rendered `App`. `use_hook` still gives the usual "create once, keep
/// across re-renders" hook behavior around it.
pub fn use_background() -> Background {
    use_hook(|| Background {
        running: Signal::new_in_scope(HashSet::new(), ScopeId::ROOT),
        installing: Signal::new_in_scope(HashMap::new(), ScopeId::ROOT),
        playtime_generation: Signal::new_in_scope(0_u32, ScopeId::ROOT),
        organisation: Signal::new_in_scope(0_u32, ScopeId::ROOT),
    })
}
