//! Downloads left unfinished, by a previous run or by what stopped them in
//! this one: listed as paused rows, and carried on by themselves once what
//! stopped them is gone (#1281). Never one the user paused, nor one a full
//! disk stopped, which waits for the user to free space and press Reanudar.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use decanter_core::Library;
use decanter_gog::{PauseReason, UnfinishedDownload};
use gpui::{App, AsyncApp};

use super::{actions, install};
use crate::state::{AppState, DownloadKey, DownloadKind, InstallStatus};

const TICK: Duration = Duration::from_secs(5);
/// A download the network stopped is tried again after this, then after
/// twice as long each time it is still down, up to [`MAX_RETRY`] (#1281).
const FIRST_RETRY: Duration = Duration::from_secs(15);
const MAX_RETRY: Duration = Duration::from_secs(5 * 60);
/// How often downloads another process runs are looked at again, to see
/// them end there.
const RESCAN: Duration = Duration::from_secs(30);
/// How long a clean quit waits for running downloads to record their pause.
pub const QUIT_WAIT: Duration = Duration::from_secs(2);

/// When a download stopped by the network or an unplugged disk is tried
/// again, so a cause that persists is retried less and less often instead of
/// in a loop.
struct Retry {
    at: Instant,
    wait: Duration,
}

impl Retry {
    fn after(now: Instant, wait: Duration) -> Self {
        Self { at: now + wait, wait }
    }

    fn is_due(&mut self, now: Instant) -> bool {
        if now < self.at {
            return false;
        }
        self.wait = (self.wait * 2).clamp(FIRST_RETRY, MAX_RETRY);
        self.at = now + self.wait;
        true
    }
}

/// Lists what the active profile's library folders hold unfinished, carries
/// on what it should, and from then on watches the paused rows: the network
/// retried with a growing wait, an unplugged library folder resumed when it
/// is back, folders plugged in later scanned, and downloads another process
/// runs followed until they end there.
pub async fn watch(state: AppState, cx: &mut AsyncApp) {
    let mut retries: HashMap<DownloadKey, Retry> = HashMap::new();
    // Each found download carries on by itself once per run at most, so one
    // that keeps failing is never started over and over.
    let mut started: HashSet<DownloadKey> = HashSet::new();
    let mut roots = None;
    let mut scanned = Instant::now();
    loop {
        let profile_id = state.session.read_with(cx, |session, _| session.active_profile.clone());
        if let Some(profile_id) = profile_id {
            let library = crate::core::library_for_profile(&profile_id)
                .inspect_err(|err| tracing::warn!(profile_id, %err, "could not resolve the library to look for unfinished downloads"))
                .ok();
            let current = library.as_ref().map(Library::roots);
            let elsewhere = state.background.read_with(cx, |background, _| {
                background
                    .installing
                    .values()
                    .any(|row| matches!(row.status, InstallStatus::Elsewhere { .. }))
            });
            if let Some(library) = library
                && (current != roots || elsewhere && scanned.elapsed() >= RESCAN)
            {
                roots = current;
                scanned = Instant::now();
                for key in scan(&library, &state, cx).await {
                    if started.insert(key.clone()) {
                        cx.update(|cx| actions::resume_download(key, cx));
                    }
                }
            }
        }
        retry_stopped(&state, &mut retries, cx);
        cx.background_executor().timer(TICK).await;
    }
}

/// Tries again the downloads the network or an unplugged library folder
/// stopped, each on its own growing wait.
fn retry_stopped(state: &AppState, retries: &mut HashMap<DownloadKey, Retry>, cx: &AsyncApp) {
    let now = Instant::now();
    let due: Vec<DownloadKey> = state.background.read_with(cx, |background, _| {
        retries.retain(|key, _| background.installing.contains_key(key));
        let mut due = Vec::new();
        for (key, row) in &background.installing {
            if row.kind == DownloadKind::Repair {
                continue;
            }
            let first_wait = match &row.status {
                // Bytes are moving again: whatever stopped it is over.
                InstallStatus::Installing { .. } => {
                    retries.remove(key);
                    continue;
                }
                InstallStatus::Paused {
                    reason: PauseReason::Network,
                    ..
                } => FIRST_RETRY,
                InstallStatus::Paused {
                    reason: PauseReason::LibraryUnavailable,
                    ..
                } if row.root.as_ref().is_some_and(|root| root.is_dir()) => Duration::ZERO,
                _ => continue,
            };
            if retries
                .entry(key.clone())
                .or_insert_with(|| Retry::after(now, first_wait))
                .is_due(now)
            {
                due.push(key.clone());
            }
        }
        due
    });
    for key in due {
        tracing::info!(
            account_id = key.account_id,
            product_id = key.product_id,
            "retrying a download the network or an unplugged library stopped"
        );
        cx.update(|cx| actions::resume_download(key, cx));
    }
}

/// Lists what `library`'s folders hold unfinished. A row already listed
/// stays as it is, but for one another process ran, which takes what its
/// journal says now. Returns the rows new to the list that carry on by
/// themselves.
async fn scan(library: &Library, state: &AppState, cx: &mut AsyncApp) -> Vec<DownloadKey> {
    let on_start = crate::core::db()
        .and_then(|db| db.resume_downloads_on_start().map_err(crate::core::StoreError::Query))
        .inspect_err(|err| tracing::warn!(%err, "could not read the resume-on-start setting, resuming"))
        .unwrap_or(true);
    let found = decanter_gog::unfinished_downloads(library).await;
    let (resume, gone) = state.background.update(cx, |background, cx| {
        let mut resume = Vec::new();
        let mut listed = HashSet::new();
        for download in found {
            let key = DownloadKey::new(&download.account_id, &download.product_id);
            listed.insert(key.clone());
            match background.installing.get_mut(&key) {
                Some(row) if matches!(row.status, InstallStatus::Elsewhere { .. }) => {
                    row.status = status_of(&download);
                }
                Some(_) => {}
                None => {
                    if !download.in_use && !download.is_repair && resumes_when_found(download.reason, on_start) {
                        resume.push(key.clone());
                    }
                    let mut row = install::queued(download.title.clone(), kind_of(library, &download));
                    row.status = status_of(&download);
                    row.root = Some(download.root.clone());
                    background.installing.insert(key, row);
                }
            }
        }
        // Its folder is gone: the other process finished or cancelled it.
        let gone: Vec<DownloadKey> = background
            .installing
            .iter()
            .filter(|(key, row)| matches!(row.status, InstallStatus::Elsewhere { .. }) && !listed.contains(*key))
            .map(|(key, _)| key.clone())
            .collect();
        for key in &gone {
            background.installing.remove(key);
        }
        cx.notify();
        (resume, gone)
    });
    for key in gone {
        tracing::info!(
            account_id = key.account_id,
            product_id = key.product_id,
            "a download another process ran has ended"
        );
        install::refresh_install_state(state, &key.account_id, &key.product_id, cx);
    }
    resume
}

/// Whether a download found stopped carries on by itself: one decanter's
/// closing or a crash stopped when the setting allows it, and one whose
/// library folder was away, which is back since it was found. The network
/// is retried by the watcher on its own wait.
const fn resumes_when_found(reason: PauseReason, on_start: bool) -> bool {
    match reason {
        PauseReason::AppClosed | PauseReason::ClosedUnexpectedly => on_start,
        PauseReason::LibraryUnavailable => true,
        PauseReason::User | PauseReason::Network | PauseReason::DiskFull => false,
    }
}

const fn status_of(download: &UnfinishedDownload) -> InstallStatus {
    if download.in_use {
        return InstallStatus::Elsewhere {
            bytes_done: download.bytes_done,
            total_bytes: download.total_bytes,
        };
    }
    InstallStatus::Paused {
        reason: download.reason,
        bytes_done: download.bytes_done,
        total_bytes: Some(download.total_bytes),
        since: download.paused_since,
    }
}

fn kind_of(library: &Library, download: &UnfinishedDownload) -> DownloadKind {
    if download.is_repair {
        DownloadKind::Repair
    } else if download.parent_id.is_some() {
        DownloadKind::Dlc
    } else if library
        .read_manifest(
            &download.account_id,
            decanter_core::Storefront::Gog,
            &download.product_id,
        )
        .is_none()
    {
        DownloadKind::Install
    } else if download.build_id.is_some() {
        DownloadKind::Rollback
    } else {
        DownloadKind::Update
    }
}

/// Quits once every running download has recorded its pause, or after
/// [`QUIT_WAIT`]: the next start lists them as closed by decanter and, with
/// the setting on, carries them on.
pub fn quit(cx: &mut App) {
    cx.spawn(async move |cx| {
        decanter_gog::pause_for_quit(QUIT_WAIT).await;
        cx.update(|cx| cx.quit());
    })
    .detach();
}
