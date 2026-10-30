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
    /// Another language's files added to a game already installed.
    pub is_language: bool,
    /// A pinned build (a rollback), which a resume keeps.
    pub build_id: Option<String>,
    /// The journal's last pause, or [`PauseReason::ClosedUnexpectedly`] when
    /// it ends without one.
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
    /// When its journal last recorded anything, which for a paused download
    /// is when it paused.
    pub paused_since: Option<SystemTime>,
    /// A DLC ticked with its game and never planned, which takes the game's reason.
    pub is_queued_behind_game: bool,
}

/// Where every download of the library folder `root` is staged, for sizing what they hold.
#[must_use]
#[inline]
pub fn downloading_dir(root: &Path) -> PathBuf {
    root.join(DOWNLOADING)
}

/// A staging folder with no journal, which nothing can resume and nothing else lists.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct StrayStaging {
    pub account_id: String,
    /// The folder's name: the product id, or `<id>-repair`.
    pub name: String,
    pub size: u64,
}

/// The staging folders of `root`'s `downloading/` that have no `download.json` and that no process
/// holds, left by versions from before journals or by cuts before the first journal write.
#[must_use]
pub fn stray_staging(root: &Path) -> Vec<StrayStaging> {
    let mut found = Vec::new();
    for account in std_subfolders(&downloading_dir(root)) {
        let Some(account_id) = account.file_name().and_then(|name| name.to_str()).map(str::to_owned) else {
            continue;
        };
        for staging in std_subfolders(&account) {
            let Some(name) = staging.file_name().and_then(|name| name.to_str()).map(str::to_owned) else {
                continue;
            };
            if journal::has_journal(&staging) || staging_in(root, &account_id, &name).ok().as_deref() != Some(&staging)
            {
                continue;
            }
            // Held: a download is starting or running there, and its journal is about to be written.
            if decanter_runtime::journal::lock(&staging).is_err() {
                continue;
            }
            let size = decanter_core::directory_size(&staging)
                .inspect_err(|err| tracing::warn!(dir = %staging.display(), %err, "couldn't measure a staging folder"))
                .unwrap_or(0);
            found.push(StrayStaging {
                account_id: account_id.clone(),
                name,
                size,
            });
        }
    }
    found.sort_by(|left, right| (&left.account_id, &left.name).cmp(&(&right.account_id, &right.name)));
    found
}

fn std_subfolders(dir: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
        .map(|entry| entry.path())
        .collect()
}

/// Deletes one folder [`stray_staging`] listed (#1133: the caller has asked first).
///
/// # Errors
/// When a process holds it, it has a journal by now, or it can't be removed.
pub async fn discard_stray(root: &Path, account_id: &str, name: &str) -> anyhow::Result<()> {
    let staging = crate::achv_try!(staging_in(root, account_id, name));
    crate::achv_try!(journal::discard_stray(&staging).await);
    tracing::info!(account_id, name, staging = %staging.display(), "deleted a staging folder with no journal");
    Ok(())
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
    // A DLC queued behind its game stopped when the game did, and says why the same way.
    let reasons: Vec<(String, String, PauseReason)> = found
        .iter()
        .map(|download| {
            (
                download.account_id.clone(),
                download.product_id.clone(),
                download.reason,
            )
        })
        .collect();
    for download in &mut found {
        if !download.is_queued_behind_game {
            continue;
        }
        let game = reasons.iter().find(|(account_id, product_id, _)| {
            *account_id == download.account_id && Some(product_id) == download.parent_id.as_ref()
        });
        if let Some((_, _, reason)) = game {
            download.reason = *reason;
        }
    }
    tracing::info!(count = found.len(), "scanned for unfinished downloads");
    found
}

async fn unfinished(root: &Path, staging: &Path) -> Option<UnfinishedDownload> {
    let Some(found) = journal::inspect(staging).await else {
        return None;
    };
    let done = found.bytes_done();
    let journal::Found {
        request,
        header,
        folded,
        stop,
        in_use,
        ..
    } = found;
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
    let total_bytes = header.as_ref().map(|planned| planned.total_bytes);
    let (logged, bytes_done) = match &folded {
        // The library already records it; only the folder was left.
        Some(folded) if folded.phase == Phase::Recorded => return None,
        Some(folded) if folded.phase == Phase::Moving => (PauseReason::ClosedUnexpectedly, total_bytes.unwrap_or(0)),
        Some(folded) => (folded.paused.unwrap_or(PauseReason::ClosedUnexpectedly), done),
        None => (PauseReason::ClosedUnexpectedly, 0),
    };
    // A stop recorded while it waited is newer than the log's last record.
    let reason = stop.unwrap_or(logged);
    let room = folded
        .as_ref()
        .and_then(|stopped| stopped.room)
        .filter(|_| stop.is_none());
    Some(UnfinishedDownload {
        account_id: request.account_id.clone(),
        product_id: request.product_id.clone(),
        parent_id: request.parent_id.clone(),
        title: request.title.clone(),
        is_repair: request.kind == Kind::Repair,
        is_language: request.is_only_language,
        build_id: request.build_id.clone(),
        reason,
        room,
        bytes_done,
        total_bytes,
        root: root.to_path_buf(),
        in_use,
        paused_since: journal::last_written(staging).await,
        is_queued_behind_game: request.parent_id.is_some() && header.is_none() && stop.is_none(),
    })
}

/// Why `account_id`'s download of `product_id` is stopped, and in which
/// library folder, when it has one left unfinished.
pub(super) async fn stopped_reason(
    library: &Library,
    account_id: &str,
    product_id: &str,
) -> Option<(PauseReason, PathBuf)> {
    for root in library.roots() {
        let Ok(staging) = staging_in(&root, account_id, product_id) else {
            continue;
        };
        if let Some(download) = unfinished(&root, &staging).await {
            return Some((download.reason, root));
        }
    }
    None
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
        let bytes_done = found.bytes_done();
        let request = found.request;
        tracing::info!(account_id, product_id, root = %root.display(), "resuming an unfinished download");
        let library = library.clone().targeting(&root);
        let outcome = if request.is_only_language {
            super::language::add_language_request(&library, request, on_progress).await
        } else {
            super::install_request(&library, request, on_progress).await
        };
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
        let bytes_done = found.bytes_done();
        let broken: Vec<BrokenFile> = match found.header {
            Some(header) => header
                .files
                .into_iter()
                .map(|file| BrokenFile {
                    relative_path: file.path,
                    // A repair journalled before owners were recorded is planned from the game's own depots.
                    owner: file.owner.unwrap_or_else(|| product_id.to_owned()),
                    reason: BrokenReason::Mismatched,
                })
                .collect(),
            None => found
                .files
                .into_iter()
                .map(|file| BrokenFile {
                    relative_path: file.path,
                    owner: file.owner,
                    reason: BrokenReason::Mismatched,
                })
                .collect(),
        };
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
pub(super) fn is_offline(err: &anyhow::Error) -> bool {
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
    super::queue::forget(&(account_id.to_owned(), product_id.to_owned())).await;
    Ok(())
}
