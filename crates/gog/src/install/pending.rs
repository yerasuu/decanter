//! A download asked for but not planned yet, or stopped while it waited: `pending.json` beside its
//! journal, so it outlives a quit, a crash or a pause that came before any plan, and offline.
//!
//! It never speaks for the bytes: whether a resume trusts what is staged is the log's call alone,
//! so a stop recorded here can't hide an unclean exit from the re-hash. Written only while this
//! process holds the staging lock and nothing downloads, so its stop is newer than the log's last
//! record; opening the journal for a download removes it.

use std::io;
use std::path::{Path, PathBuf};

use anyhow::Context as _;
use decanter_runtime::journal::StagingLock;
use decanter_runtime::sanitize_relative_path;
use serde::{Deserialize, Serialize};

use super::journal::{self, PauseReason, Request};

pub(super) const PENDING: &str = "pending.json";
const FORMAT: u32 = 1;

#[derive(Serialize, Deserialize)]
pub(super) struct Pending {
    format: u32,
    pub(super) request: Request,
    /// What a repair repairs, since its request alone doesn't say.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(super) files: Vec<PendingFile>,
    /// Why it stopped before it was planned again.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) stop: Option<PauseReason>,
}

#[derive(Clone, Serialize, Deserialize)]
pub(super) struct PendingFile {
    pub(super) path: String,
    pub(super) owner: String,
}

impl Pending {
    pub(super) const fn new(request: Request, files: Vec<PendingFile>) -> Self {
        Self {
            format: FORMAT,
            request,
            files,
            stop: None,
        }
    }
}

/// A staging folder this process has entered a download into.
pub(super) struct Entered {
    pub(super) staging: PathBuf,
    /// `None` when another process holds the folder, or it couldn't be locked: the journal's own
    /// open then waits for it as before.
    pub(super) lock: Option<StagingLock>,
}

/// Takes `staging` for the download `pending` describes until it ends, so another process lists
/// it as running there. A folder without a plan gets `pending` written down; one with a plan
/// already has the request, and loses an earlier stop, since the download is asked for again.
pub(super) async fn enter(staging: &Path, pending: &Pending) -> Entered {
    let lock = match take(staging).await {
        Ok(lock) => Some(lock),
        Err(err) => {
            tracing::warn!(staging = %staging.display(), error = %err, "couldn't record the queued download");
            None
        }
    };
    if lock.is_some() {
        let recorded = if journal::has_plan(staging).await {
            clear(staging).await
        } else {
            write(staging, pending).await
        };
        if let Err(err) = recorded {
            tracing::warn!(staging = %staging.display(), error = %err, "couldn't record the queued download");
        }
    }
    Entered {
        staging: staging.to_path_buf(),
        lock,
    }
}

async fn take(staging: &Path) -> anyhow::Result<StagingLock> {
    crate::achv_try!(
        tokio::fs::create_dir_all(staging)
            .await
            .with_context(|| format!("creating {}", staging.display()))
    );
    Ok(crate::achv_try!(journal::lock_waiting(staging).await))
}

/// Records why a download stopped before any plan, keeping the request it needs to be planned
/// again. Only with `staging`'s lock held.
pub(super) async fn stopped(_held: &StagingLock, staging: &Path, mut pending: Pending, reason: PauseReason) {
    pending.stop = Some(reason);
    if let Err(err) = write(staging, &pending).await {
        tracing::warn!(staging = %staging.display(), error = %err, "couldn't record why the queued download stopped");
    }
}

async fn write(staging: &Path, pending: &Pending) -> anyhow::Result<()> {
    let encoded = crate::achv_try!(serde_json::to_vec(pending).context("encoding the queued download"));
    decanter_runtime::journal::write_atomic(&staging.join(PENDING), &encoded)
        .await
        .context("writing the queued download")
}

/// Forgets the stop and the request once the journal holds the download, before any byte moves,
/// so a crash from then on reads as one.
pub(super) async fn clear(staging: &Path) -> anyhow::Result<()> {
    match tokio::fs::remove_file(staging.join(PENDING)).await {
        Ok(()) => decanter_runtime::journal::sync_dir(staging)
            .await
            .context("syncing the download folder"),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(anyhow::Error::new(err).context("removing the queued download")),
    }
}

/// `None` when there is none, or it can't be read; checked here, where it comes back from disk.
pub(super) async fn read(staging: &Path) -> Option<Pending> {
    let bytes = match tokio::fs::read(staging.join(PENDING)).await {
        Ok(bytes) => bytes,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return None,
        Err(err) => {
            tracing::warn!(staging = %staging.display(), error = %err, "skipping an unreadable queued download");
            return None;
        }
    };
    let pending: Pending = match serde_json::from_slice(&bytes) {
        Ok(pending) => pending,
        Err(err) => {
            tracing::warn!(staging = %staging.display(), error = %err, "skipping an unreadable queued download");
            return None;
        }
    };
    let is_safe = |path: &str| sanitize_relative_path(path).is_some_and(|safe| safe.as_os_str() == path);
    if pending.format != FORMAT || pending.files.iter().any(|file| !is_safe(&file.path)) {
        tracing::warn!(staging = %staging.display(), format = pending.format, "skipping a queued download this version can't use");
        return None;
    }
    Some(pending)
}
