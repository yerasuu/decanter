//! Downloads a run left unfinished — paused, stopped by the network or a
//! full disk, or cut short by a crash — found from their journals in each
//! library folder's `downloading/`, the only place they are recorded.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use decanter_core::Library;

use super::journal::{self, Kind, PauseReason, Paused, Phase, Room};
use super::staging::{DOWNLOADING, staging_in};
use super::{BrokenFile, BrokenReason, Cancelled, Progress};

/// A download whose journal says it hasn't finished.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct UnfinishedDownload {
    pub account_id: String,
    pub product_id: String,
    /// The base game, for a DLC.
    pub parent_id: Option<String>,
    pub title: String,
    /// Carried on through [`resume_repair`] rather than [`resume`].
    pub is_repair: bool,
    /// A pinned build (a rollback), which a resume keeps.
    pub build_id: Option<String>,
    /// The journal's last pause, or [`PauseReason::ClosedUnexpectedly`] when
    /// it ends without one.
    pub reason: PauseReason,
    /// For a full disk, how much it needed.
    pub room: Option<Room>,
    pub bytes_done: u64,
    pub total_bytes: u64,
    /// The library folder it downloads into.
    pub root: PathBuf,
    /// Another decanter process (the CLI, say) is running it right now.
    pub in_use: bool,
    /// When its journal last recorded anything, which for a paused download
    /// is when it paused.
    pub paused_since: Option<SystemTime>,
}

/// Where every download of the library folder `root` is staged, for sizing what they hold.
#[must_use]
#[inline]
pub fn downloading_dir(root: &Path) -> PathBuf {
    root.join(DOWNLOADING)
}

/// Every unfinished download in `library`'s folders, for the app to list as
/// paused when it starts. A staging folder without a journal (from before
/// journals) isn't one: nothing says what it holds.
pub async fn unfinished_downloads(library: &Library) -> Vec<UnfinishedDownload> {
    let mut found = Vec::new();
    for root in library.roots() {
        for account in subfolders(&root.join(DOWNLOADING)).await {
            for staging in subfolders(&account).await {
                if let Some(download) = unfinished(&root, &staging).await {
                    found.push(download);
                }
            }
        }
    }
    tracing::info!(count = found.len(), "scanned for unfinished downloads");
    found
}

async fn unfinished(root: &Path, staging: &Path) -> Option<UnfinishedDownload> {
    let Some(journal::Found { header, folded, in_use }) = journal::inspect(staging).await else {
        return None;
    };
    let request = &header.request;
    let expected_dir = match request.kind {
        Kind::Install => request.product_id.clone(),
        Kind::Repair => format!("{}-repair", request.product_id),
    };
    // Found by its folder, so a header naming another download was copied
    // or edited by hand, and resuming it would write somewhere else.
    if staging_in(root, &request.account_id, &expected_dir).ok().as_deref() != Some(staging) {
        tracing::warn!(staging = %staging.display(), "download journal names another download, skipping it");
        return None;
    }
    let (reason, bytes_done) = match &folded {
        // The library already records it; only the folder was left.
        Some(folded) if folded.phase == Phase::Recorded => return None,
        Some(folded) if folded.phase == Phase::Moving => (PauseReason::ClosedUnexpectedly, header.total_bytes),
        Some(folded) => (
            folded.paused.unwrap_or(PauseReason::ClosedUnexpectedly),
            folded.bytes_done(&header),
        ),
        None => (PauseReason::ClosedUnexpectedly, 0),
    };
    let room = folded.as_ref().and_then(|stopped| stopped.room);
    Some(UnfinishedDownload {
        account_id: request.account_id.clone(),
        product_id: request.product_id.clone(),
        parent_id: request.parent_id.clone(),
        title: request.title.clone(),
        is_repair: request.kind == Kind::Repair,
        build_id: request.build_id.clone(),
        reason,
        room,
        bytes_done,
        total_bytes: header.total_bytes,
        root: root.to_path_buf(),
        in_use,
        paused_since: journal::last_written(staging).await,
    })
}

async fn subfolders(dir: &Path) -> Vec<PathBuf> {
    let mut entries = match tokio::fs::read_dir(dir).await {
        Ok(entries) => entries,
        Err(err) => {
            if err.kind() != std::io::ErrorKind::NotFound {
                tracing::warn!(dir = %dir.display(), error = %err, "couldn't list unfinished downloads");
            }
            return Vec::new();
        }
    };
    let mut folders = Vec::new();
    loop {
        match entries.next_entry().await {
            Ok(Some(entry)) => {
                if entry.file_type().await.is_ok_and(|kind| kind.is_dir()) {
                    folders.push(entry.path());
                }
            }
            Ok(None) => break,
            Err(err) => {
                tracing::warn!(dir = %dir.display(), error = %err, "couldn't list unfinished downloads");
                break;
            }
        }
    }
    folders
}

/// What [`resume`] fails with when no journal of the download is found.
#[derive(Debug, thiserror::Error)]
#[error("there is no unfinished download of it to resume")]
#[non_exhaustive]
pub struct NothingToResume;

/// Carries on an unfinished download, in whichever library folder holds it.
///
/// It is planned again from the request its journal recorded, since chunk
/// URLs expire; a build GOG published meanwhile keeps the chunks it shares.
///
/// # Errors
/// [`NothingToResume`] when there is no journal of it; [`Paused`] for the
/// network when GOG couldn't be reached to plan it again, keeping it as it
/// was; otherwise as [`super::install`].
pub async fn resume(
    library: &Library,
    account_id: &str,
    product_id: &str,
    on_progress: impl FnMut(Progress) + Send + 'static,
) -> anyhow::Result<()> {
    for root in library.roots() {
        let staging = crate::achv_try!(staging_in(&root, account_id, product_id));
        let Some(found) = journal::inspect(&staging).await else {
            continue;
        };
        let bytes_done = found
            .folded
            .as_ref()
            .map_or(0, |folded| folded.bytes_done(&found.header));
        let request = found.header.request;
        tracing::info!(account_id, product_id, root = %root.display(), "resuming an unfinished download");
        let outcome = super::install(
            &library.clone().targeting(&root),
            account_id,
            product_id,
            request.parent_id.as_deref(),
            &request.title,
            request.branch.as_deref(),
            request.build_id.as_deref(),
            on_progress,
        )
        .await;
        return still_paused(outcome, bytes_done, root);
    }
    Err(NothingToResume.into())
}

/// Carries on an unfinished repair, in whichever library folder holds it:
/// the files its journal lists are planned again from the depot each came
/// from, at the build the game is on now.
///
/// # Errors
/// As [`resume`], and as [`super::repair`].
pub async fn resume_repair(
    library: &Library,
    account_id: &str,
    product_id: &str,
    on_progress: impl FnMut(Progress) + Send + 'static,
) -> anyhow::Result<()> {
    for root in library.roots() {
        let staging = crate::achv_try!(staging_in(&root, account_id, &format!("{product_id}-repair")));
        let Some(found) = journal::inspect(&staging).await else {
            continue;
        };
        let bytes_done = found
            .folded
            .as_ref()
            .map_or(0, |folded| folded.bytes_done(&found.header));
        let broken: Vec<BrokenFile> = found
            .header
            .files
            .into_iter()
            .map(|file| BrokenFile {
                relative_path: file.path,
                // A repair journalled before owners were recorded is planned from the game's own depots.
                owner: file.owner.unwrap_or_else(|| product_id.to_owned()),
                reason: BrokenReason::Mismatched,
            })
            .collect();
        tracing::info!(account_id, product_id, root = %root.display(), files = broken.len(), "resuming an unfinished repair");
        let outcome = super::repair(
            &library.clone().targeting(&root),
            account_id,
            product_id,
            &broken,
            on_progress,
        )
        .await;
        return still_paused(outcome, bytes_done, root);
    }
    Err(NothingToResume.into())
}

/// Before any byte moves a resume asks GOG for a session and a plan; offline,
/// that is the same pause the download was in, not a failure that would end
/// retrying it.
fn still_paused(outcome: anyhow::Result<()>, bytes_done: u64, root: PathBuf) -> anyhow::Result<()> {
    match outcome {
        Err(err) if is_offline(&err) => {
            tracing::info!(error = %err, "GOG unreachable while planning a resume, still paused");
            Err(Paused {
                reason: PauseReason::Network,
                bytes_done,
                root: Some(root),
                room: None,
            }
            .into())
        }
        other => other,
    }
}

/// GOG or its CDN didn't answer, as opposed to refusing or no longer offering what was asked.
fn is_offline(err: &anyhow::Error) -> bool {
    if err.is::<Paused>() || err.is::<Cancelled>() {
        return false;
    }
    // A 404 says the build or depot is gone, which waiting won't bring back.
    err.chain()
        .any(|cause| match cause.downcast_ref::<crate::api::client::Error>() {
            Some(crate::api::client::Error::Network { .. }) => true,
            Some(crate::api::client::Error::Unavailable { status, .. }) => *status == 429 || *status >= 500,
            _ => cause.downcast_ref::<reqwest::Error>().is_some(),
        })
}

/// Deletes what `account_id`'s unfinished download of `product_id` (and of
/// its repair) had fetched, in every library folder: the end of a confirmed
/// cancel (#1133) of a download nothing is running.
///
/// # Errors
/// When another decanter process runs it, or it can't be removed.
pub async fn discard(library: &Library, account_id: &str, product_id: &str) -> anyhow::Result<()> {
    for root in library.roots() {
        for folder in [product_id.to_owned(), format!("{product_id}-repair")] {
            let staging = crate::achv_try!(staging_in(&root, account_id, &folder));
            if !tokio::fs::try_exists(&staging).await.unwrap_or(true) {
                continue;
            }
            crate::achv_try!(journal::discard(&staging).await);
            tracing::info!(account_id, product_id, staging = %staging.display(), "discarded an unfinished download");
        }
    }
    Ok(())
}
