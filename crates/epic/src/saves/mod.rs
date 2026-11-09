// SPDX-License-Identifier: GPL-3.0-or-later
//! An Epic title's cloud saves (spec, section 9): the snapshots Epic keeps of a save folder, each a
//! manifest plus chunks in the format of a game build (sections 6 and 7), listed, restored and
//! uploaded against the folder the catalog's `CloudSaveFolder` names.
//!
//! These calls write to the user's Epic account, so the rules are narrower than GOG's sync:
//!
//! - nothing is ever deleted, remotely or locally: an upload adds a snapshot beside the old ones and
//!   puts only chunks the listing does not hold yet, and a restore writes the snapshot's files over
//!   their local copies and leaves every other local file alone;
//! - a direction moves on its own only when the other side has not changed since the last clean
//!   sync and holds nothing newer (spec 9's newest-manifest-time against newest-local-file-time,
//!   with its 60-second window); any other case is [`SaveError::NeedsConfirmation`], which only an
//!   [`Approval::Confirmed`] naming the snapshot the user was shown gets past;
//! - what a write would move is logged, as counts and sizes, before the first byte moves, and every
//!   chunk and file is checked before any of them is written or put.
//!
//! What the remote paths' first two segments are is UNVERIFIED (spec, Q22). The reading here: a
//! key is `<a>/<b>/<appName>/manifests/<YYYY.MM.DD-HH.MM.SS>.manifest` or
//! `<a>/<b>/<appName>/<ChunksVn>/…/<chunk>.chunk`, and a snapshot's chunks sit under the same first
//! three segments as its manifest. A listing that does not fit is refused as
//! [`SaveError::UnexpectedShape`], with a log of what did not fit, and nothing moves.

mod folder;
mod pack;
mod remote;

use std::collections::{BTreeMap, HashMap};
use std::fmt;

use chrono::Utc;
use tokio::task::spawn_blocking;

pub use self::folder::{FolderFault, SaveFolder, SaveRoots, SaveSpec, save_folder};
use self::folder::{LocalFile, Scan};
use self::remote::{Listing, SnapshotEntry};
use crate::install::chunk::{self, Want};
use crate::library::is_id;
use crate::manifest::{MAX_CHUNK_FILE, Manifest, ManifestFile, Sha1Digest, parse_manifest};

/// Spec 9: a snapshot and the newest local file this close in time are the same age.
const SAME_AGE_SECS: u64 = 60;
/// A save folder past these is refused rather than read into memory or pushed to the account.
const MAX_SAVE_FILES: usize = 10_000;
const MAX_SAVE_BYTES: u64 = 256 << 20;

/// How a save folder stands against its newest snapshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum SaveVerdict {
    /// Neither side has a save.
    NoSaves,
    /// Only the cloud has one: a restore overwrites nothing.
    RemoteOnly,
    /// Only the folder has one: an upload supersedes nothing.
    LocalOnly,
    /// The newest snapshot and the newest local file are within spec 9's 60 seconds, or neither
    /// changed since the last clean sync.
    SameAge,
    /// The cloud changed since the last clean sync and the folder did not.
    RemoteNewer,
    /// The folder changed since the last clean sync and the cloud did not.
    LocalNewer,
    /// Both changed, or no sync is recorded to tell which one did.
    Conflict,
}

impl SaveVerdict {
    #[inline]
    #[must_use]
    pub const fn is_download_safe(self) -> bool {
        matches!(self, Self::RemoteOnly | Self::RemoteNewer)
    }

    #[inline]
    #[must_use]
    pub const fn is_upload_safe(self) -> bool {
        matches!(self, Self::LocalOnly | Self::LocalNewer)
    }
}

/// The newest snapshot the listing holds.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Snapshot {
    /// The manifest's file name, which an [`Approval::Confirmed`] names back.
    pub name: String,
    /// Unix seconds, from the name (spec, 9).
    pub time: i64,
}

/// What [`check_saves`] found, without moving anything.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct SaveCheck {
    pub verdict: SaveVerdict,
    pub snapshot: Option<Snapshot>,
    pub snapshots: usize,
    /// The folder's files the catalog's filters keep, and their size.
    pub local_files: usize,
    pub local_bytes: u64,
    /// Unix seconds of the newest of them.
    pub local_newest: Option<i64>,
}

/// Who decided a transfer may go ahead.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Approval {
    /// No one: it goes ahead only when its [`SaveVerdict`] says it is safe.
    Automatic,
    /// The user, after being told what the transfer replaces; `seen` is the snapshot they were shown,
    /// and a listing whose newest snapshot is another one refuses it as [`SaveError::Changed`].
    Confirmed { seen: Option<String> },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Direction {
    Download,
    Upload,
}

/// What a transfer did.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct SaveOutcome {
    pub verdict: SaveVerdict,
    /// `None` when nothing moved.
    pub moved: Option<Direction>,
    pub files: usize,
    pub bytes: u64,
    /// The instant to record as the game's last clean sync, `None` when there is none to record.
    pub last_sync: Option<i64>,
}

/// Why nothing, or not everything, moved. No variant carries a URL, a token or save content.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum SaveError {
    #[error("the save folder is unusable: {0}")]
    Folder(#[from] FolderFault),
    #[error("Epic closed the session: log in to Epic Games again in Settings")]
    SessionClosed,
    /// Epic or a link it gave did not answer; nothing about the saves is known.
    #[error("Epic could not be asked about the cloud saves: {0}")]
    Unreachable(String),
    /// Epic answered and turned the request down.
    #[error("Epic refused the cloud saves request: {0}")]
    Refused(String),
    /// The listing or an answer is not the shape spec 9 describes; see the log for the counts.
    #[error("Epic's cloud saves answer has an unexpected shape ({0}); nothing was changed")]
    UnexpectedShape(&'static str),
    /// The newest snapshot names chunks the listing does not hold.
    #[error("the newest cloud save lacks {missing} of its chunks; nothing was changed")]
    Incomplete { missing: usize },
    #[error("the cloud save is corrupt: {0}; nothing was changed")]
    Corrupt(String),
    #[error("the save is larger than decanter syncs ({what} past {limit})")]
    TooLarge { what: &'static str, limit: u64 },
    #[error("reading or writing the save folder failed: {0}")]
    Local(String),
    /// The transfer would replace newer or unrelated data and was not confirmed.
    #[error("the cloud saves need a decision ({verdict:?}); nothing was changed")]
    NeedsConfirmation {
        verdict: SaveVerdict,
        snapshot: Option<Snapshot>,
    },
    /// The cloud's newest snapshot is not the one the user confirmed against.
    #[error("the cloud saves changed since they were checked; nothing was changed")]
    Changed,
}

impl SaveError {
    /// Whether the same call may succeed later with nothing changed on decanter's side.
    #[inline]
    #[must_use]
    pub const fn is_retryable(&self) -> bool {
        matches!(
            self,
            Self::Unreachable(_) | Self::UnexpectedShape(_) | Self::Incomplete { .. } | Self::Changed
        )
    }
}

/// A transfer's game and folder.
#[derive(Clone, Copy)]
pub struct SaveTarget<'target> {
    account_id: &'target str,
    app_name: &'target str,
    folder: &'target SaveFolder,
}

impl<'target> SaveTarget<'target> {
    /// # Errors
    ///
    /// An id that cannot name an Epic account or title.
    #[inline]
    pub fn new(
        account_id: &'target str,
        app_name: &'target str,
        folder: &'target SaveFolder,
    ) -> Result<Self, SaveError> {
        if !is_id(account_id) || !is_id(app_name) {
            return Err(SaveError::Folder(FolderFault::BadId));
        }
        Ok(Self {
            account_id,
            app_name,
            folder,
        })
    }
}

/// Lists the cloud and scans the folder, and says how they stand. Reads only.
///
/// # Errors
///
/// Every [`SaveError`] but the confirmation ones.
#[inline]
pub async fn check_saves(target: SaveTarget<'_>, last_sync: Option<i64>) -> Result<SaveCheck, SaveError> {
    let (listing, _) = remote::list(target.account_id, target.app_name).await?;
    let scan = scan(target.folder).await?;
    let check = assess(&listing, &scan, last_sync);
    tracing::info!(
        account_id = target.account_id,
        app_name = target.app_name,
        verdict = ?check.verdict,
        snapshots = check.snapshots,
        snapshot_time = check.snapshot.as_ref().map(|snapshot| snapshot.time),
        local_files = check.local_files,
        local_bytes = check.local_bytes,
        local_newest = check.local_newest,
        "checked epic cloud saves"
    );
    Ok(check)
}

/// Restores the newest snapshot into the folder.
///
/// # Errors
///
/// [`SaveError::NeedsConfirmation`] when an [`Approval::Automatic`] restore would overwrite a
/// folder that changed, and every other [`SaveError`]; on any error no local file was written.
#[inline]
pub async fn download_saves(
    target: SaveTarget<'_>,
    last_sync: Option<i64>,
    approval: Approval,
) -> Result<SaveOutcome, SaveError> {
    let (listing, _) = remote::list(target.account_id, target.app_name).await?;
    let scan = scan(target.folder).await?;
    let check = assess(&listing, &scan, last_sync);
    if let Some(outcome) = gate(&check, &approval, Direction::Download, &scan)? {
        return Ok(outcome);
    }
    let Some(newest) = listing.newest() else {
        return Ok(unmoved(check.verdict, None));
    };
    restore(target, &listing, newest, &scan, check.verdict).await
}

/// Uploads the folder as a new snapshot.
///
/// # Errors
///
/// [`SaveError::NeedsConfirmation`] when an [`Approval::Automatic`] upload would supersede a cloud
/// that changed, and every other [`SaveError`]; on an error before the first upload nothing was
/// put, and one after leaves only chunks no snapshot names yet.
#[inline]
pub async fn upload_saves(
    target: SaveTarget<'_>,
    last_sync: Option<i64>,
    approval: Approval,
) -> Result<SaveOutcome, SaveError> {
    let (listing, session) = remote::list(target.account_id, target.app_name).await?;
    let scan = scan(target.folder).await?;
    let check = assess(&listing, &scan, last_sync);
    if let Some(outcome) = gate(&check, &approval, Direction::Upload, &scan)? {
        return Ok(outcome);
    }
    let included: Vec<LocalFile> = scan.files.into_iter().filter(|file| file.is_included).collect();
    if included.is_empty() {
        return Ok(unmoved(check.verdict, None));
    }
    let app_name = target.app_name.to_owned();
    let packed = spawn_blocking(move || pack::pack(&app_name, &included))
        .await
        .map_err(|_| SaveError::Local("the save packing task panicked".to_owned()))??;
    let started = Utc::now();
    let name = format!("{}.manifest", started.format("%Y.%m.%d-%H.%M.%S"));
    if listing.has_snapshot(&name) {
        return Err(SaveError::Changed);
    }
    let prefix = listing.newest().map(|newest| newest.prefix.as_str());
    let manifest_path = format!("manifests/{name}");
    let chunks_total = packed.chunks.len();
    let mut wanted = Vec::with_capacity(chunks_total);
    let mut upload_bytes = 0_u64;
    for chunk in packed.chunks {
        // Chunks are named by their content, so one the listing holds is this very chunk and is
        // neither put again nor overwritten.
        if prefix.is_some_and(|prefix| listing.has_chunk(prefix, &chunk.path)) {
            continue;
        }
        upload_bytes = upload_bytes.saturating_add(u64::try_from(chunk.file.len()).unwrap_or(u64::MAX));
        wanted.push(chunk);
    }
    tracing::info!(
        account_id = target.account_id,
        app_name = target.app_name,
        verdict = ?check.verdict,
        files = packed.files,
        bytes = packed.bytes,
        chunks = chunks_total,
        chunks_already_in_cloud = chunks_total.saturating_sub(wanted.len()),
        chunks_to_upload = wanted.len(),
        upload_bytes,
        manifest_bytes = packed.manifest.len(),
        snapshot = name,
        "epic cloud saves upload planned"
    );
    let mut paths: Vec<&str> = wanted.iter().map(|chunk| chunk.path.as_str()).collect();
    paths.push(&manifest_path);
    let links = remote::write_links(target.account_id, target.app_name, session, &paths).await?;
    let client = remote::anonymous()?;
    let uploaded = wanted.len();
    for chunk in wanted {
        let link = links
            .get(chunk.path.as_str())
            .ok_or(SaveError::UnexpectedShape("an upload link is missing"))?;
        remote::put(&client, link, chunk.file).await?;
    }
    // Last, so that a snapshot never names a chunk that is not up yet.
    let link = links
        .get(manifest_path.as_str())
        .ok_or(SaveError::UnexpectedShape("an upload link is missing"))?;
    remote::put(&client, link, packed.manifest).await?;
    tracing::info!(
        account_id = target.account_id,
        app_name = target.app_name,
        files = packed.files,
        bytes = packed.bytes,
        chunks_uploaded = uploaded,
        snapshot = name,
        "epic cloud saves uploaded"
    );
    Ok(SaveOutcome {
        verdict: check.verdict,
        moved: Some(Direction::Upload),
        files: packed.files,
        bytes: packed.bytes,
        last_sync: Some(Utc::now().timestamp().max(started.timestamp())),
    })
}

async fn scan(folder: &SaveFolder) -> Result<Scan, SaveError> {
    let owned = folder.clone();
    spawn_blocking(move || folder::scan(&owned))
        .await
        .map_err(|_| SaveError::Local("the save scan task panicked".to_owned()))?
}

fn assess(listing: &Listing, scan: &Scan, last_sync: Option<i64>) -> SaveCheck {
    let snapshot = listing.newest().map(|newest| Snapshot {
        name: newest.name.clone(),
        time: newest.time,
    });
    let verdict = verdict(snapshot.as_ref().map(|snapshot| snapshot.time), scan.newest, last_sync);
    SaveCheck {
        verdict,
        snapshot,
        snapshots: listing.snapshots.len(),
        local_files: scan.included,
        local_bytes: scan.included_bytes,
        local_newest: scan.newest,
    }
}

/// Spec 9's comparison of the newest snapshot with the newest local file, each side read as changed
/// when it is newer than `last_sync`, or always when no sync is recorded.
fn verdict(remote: Option<i64>, local: Option<i64>, last_sync: Option<i64>) -> SaveVerdict {
    let (remote, local) = match (remote, local) {
        (None, None) => return SaveVerdict::NoSaves,
        (Some(_), None) => return SaveVerdict::RemoteOnly,
        (None, Some(_)) => return SaveVerdict::LocalOnly,
        (Some(remote), Some(local)) => (remote, local),
    };
    if remote.abs_diff(local) <= SAME_AGE_SECS {
        return SaveVerdict::SameAge;
    }
    let is_remote_changed = last_sync.is_none_or(|synced| remote > synced);
    let is_local_changed = last_sync.is_none_or(|synced| local > synced);
    match (is_remote_changed, is_local_changed) {
        // After an upload the snapshot is named at the upload's time, later than the files in it,
        // so two sides that both stayed as the last sync left them read as in step.
        (false, false) => SaveVerdict::SameAge,
        (true, false) if remote > local => SaveVerdict::RemoteNewer,
        (false, true) if local > remote => SaveVerdict::LocalNewer,
        (true, false) | (false, true) | (true, true) => SaveVerdict::Conflict,
    }
}

/// `Some` outcome when the transfer stops here without an error, `None` when it goes ahead.
fn gate(
    check: &SaveCheck,
    approval: &Approval,
    direction: Direction,
    scan: &Scan,
) -> Result<Option<SaveOutcome>, SaveError> {
    let verdict = check.verdict;
    let has_source = match direction {
        Direction::Download => check.snapshot.is_some(),
        Direction::Upload => scan.included > 0,
    };
    if !has_source {
        return Ok(Some(unmoved(verdict, None)));
    }
    match approval {
        Approval::Confirmed { seen } => {
            if seen.as_deref() != check.snapshot.as_ref().map(|snapshot| snapshot.name.as_str()) {
                return Err(SaveError::Changed);
            }
            Ok(None)
        }
        Approval::Automatic => {
            let is_safe = match direction {
                Direction::Download => verdict.is_download_safe(),
                Direction::Upload => verdict.is_upload_safe(),
            };
            if is_safe {
                Ok(None)
            } else if verdict == SaveVerdict::SameAge {
                let settled = check
                    .snapshot
                    .as_ref()
                    .map(|snapshot| snapshot.time)
                    .max(check.local_newest);
                Ok(Some(unmoved(verdict, settled)))
            } else {
                Err(SaveError::NeedsConfirmation {
                    verdict,
                    snapshot: check.snapshot.clone(),
                })
            }
        }
    }
}

const fn unmoved(verdict: SaveVerdict, last_sync: Option<i64>) -> SaveOutcome {
    SaveOutcome {
        verdict,
        moved: None,
        files: 0,
        bytes: 0,
        last_sync,
    }
}

/// What a restore writes, decided before any chunk is fetched.
struct Restore<'manifest> {
    /// Each file to write, with the local file it replaces when the folder has one by that name.
    files: Vec<(&'manifest ManifestFile, Option<&'manifest LocalFile>)>,
    unchanged: usize,
}

async fn restore(
    target: SaveTarget<'_>,
    listing: &Listing,
    newest: &SnapshotEntry,
    scan: &Scan,
    verdict: SaveVerdict,
) -> Result<SaveOutcome, SaveError> {
    let client = remote::anonymous()?;
    let raw = remote::fetch(&client, newest.link.as_ref(), remote::MAX_SNAPSHOT_MANIFEST).await?;
    let manifest = parse_manifest(&raw).map_err(|err| SaveError::Corrupt(err.to_string()))?;
    if manifest.total_size() > MAX_SAVE_BYTES {
        return Err(SaveError::TooLarge {
            what: "snapshot",
            limit: MAX_SAVE_BYTES,
        });
    }
    if manifest.files().len() > MAX_SAVE_FILES {
        return Err(SaveError::TooLarge {
            what: "snapshot files",
            limit: u64::try_from(MAX_SAVE_FILES).unwrap_or(u64::MAX),
        });
    }
    if manifest.app_name() != target.app_name {
        tracing::info!(
            app_name = target.app_name,
            "epic cloud save manifest names another app, restoring it by its listing"
        );
    }
    let plan = plan_restore(&manifest, scan).await?;
    let (bytes, overwritten) = plan
        .files
        .iter()
        .fold((0_u64, 0_usize), |(bytes, overwritten), (file, local)| {
            (
                bytes.saturating_add(file.size()),
                overwritten.saturating_add(usize::from(local.is_some())),
            )
        });
    let placements = placements(&plan);
    let keep = scan
        .files
        .len()
        .saturating_sub(overwritten)
        .saturating_sub(plan.unchanged);
    tracing::info!(
        account_id = target.account_id,
        app_name = target.app_name,
        verdict = ?verdict,
        snapshot = newest.name,
        snapshot_files = manifest.files().len(),
        unchanged = plan.unchanged,
        to_write = plan.files.len(),
        overwritten,
        new = plan.files.len().saturating_sub(overwritten),
        bytes,
        chunks_to_fetch = placements.len(),
        local_files_kept = keep,
        "epic cloud saves restore planned"
    );
    if plan.files.is_empty() {
        return Ok(SaveOutcome {
            verdict,
            moved: None,
            files: 0,
            bytes: 0,
            last_sync: Some(Utc::now().timestamp().max(newest.time)),
        });
    }
    let bodies = assemble(&client, listing, newest, &manifest, &plan, &placements).await?;
    let mut writes = Vec::with_capacity(plan.files.len());
    for ((file, local), body) in plan.files.iter().zip(bodies) {
        writes.push(folder::Write {
            relative: file.path().clone(),
            existing: local.map(|local| local.path.clone()),
            body,
        });
    }
    let root = target.folder.clone();
    let time = newest.time;
    let written = spawn_blocking(move || folder::write_all(&root, writes, time))
        .await
        .map_err(|_| SaveError::Local("the save writing task panicked".to_owned()))??;
    tracing::info!(
        account_id = target.account_id,
        app_name = target.app_name,
        files = written,
        bytes,
        snapshot = newest.name,
        "epic cloud saves restored"
    );
    Ok(SaveOutcome {
        verdict,
        moved: Some(Direction::Download),
        files: written,
        bytes,
        last_sync: Some(Utc::now().timestamp().max(newest.time)),
    })
}

/// Which of the snapshot's files differ from the folder, matched by name ignoring case as the
/// Windows game would open them.
async fn plan_restore<'manifest>(
    manifest: &'manifest Manifest,
    scan: &'manifest Scan,
) -> Result<Restore<'manifest>, SaveError> {
    let local: HashMap<String, &LocalFile> = scan.files.iter().map(|file| (file.relative.folded(), file)).collect();
    let mut files = Vec::new();
    let mut unchanged = 0_usize;
    for file in manifest.files() {
        let existing = local.get(&file.path().folded()).copied();
        if let Some(found) = existing
            && found.size == file.size()
        {
            let path = found.path.clone();
            let sha1 = spawn_blocking(move || folder::sha1_of(&path))
                .await
                .map_err(|_| SaveError::Local("the save hashing task panicked".to_owned()))??;
            if sha1 == *file.sha1() {
                unchanged = unchanged.saturating_add(1);
                continue;
            }
        }
        files.push((file, existing));
    }
    Ok(Restore { files, unchanged })
}

/// Where one part of a chunk goes: `size` bytes from `offset` in the chunk's data to `at` in the
/// body of `plan.files[file]`.
struct Placement {
    file: usize,
    at: usize,
    offset: usize,
    size: usize,
}

/// Every chunk the files of `plan` need, with where each of its parts goes.
fn placements(plan: &Restore<'_>) -> BTreeMap<usize, Vec<Placement>> {
    let mut by_chunk: BTreeMap<usize, Vec<Placement>> = BTreeMap::new();
    for (index, (file, _)) in plan.files.iter().enumerate() {
        let mut at = 0_usize;
        for part in file.parts() {
            let size = usize::try_from(part.size).unwrap_or(usize::MAX);
            by_chunk.entry(part.chunk).or_default().push(Placement {
                file: index,
                at,
                offset: usize::try_from(part.offset).unwrap_or(usize::MAX),
                size,
            });
            at = at.saturating_add(size);
        }
    }
    by_chunk
}

/// Every file of `plan` assembled from its chunks and checked against its SHA-1. Each chunk is
/// fetched once and copied to every place that names it, so memory holds the files and one chunk.
async fn assemble(
    client: &reqwest::Client,
    listing: &Listing,
    newest: &SnapshotEntry,
    manifest: &Manifest,
    plan: &Restore<'_>,
    placements: &BTreeMap<usize, Vec<Placement>>,
) -> Result<Vec<Vec<u8>>, SaveError> {
    let feature_level = manifest.feature_level();
    let mut fetches = Vec::with_capacity(placements.len());
    let mut missing = 0_usize;
    for (&index, places) in placements {
        let found = manifest.chunks().get(index).and_then(|chunk| {
            listing
                .chunk_link(&newest.prefix, &chunk::path(feature_level, chunk))
                .map(|link| (chunk, link, places))
        });
        match found {
            Some(fetch) => fetches.push(fetch),
            None => missing = missing.saturating_add(1),
        }
    }
    if missing > 0 {
        tracing::warn!(
            snapshot = newest.name,
            missing,
            needed = placements.len(),
            listed_chunks = listing.chunk_count(),
            "epic cloud save snapshot names chunks the listing lacks, nothing restored"
        );
        return Err(SaveError::Incomplete { missing });
    }
    let mut bodies: Vec<Vec<u8>> = plan
        .files
        .iter()
        .map(|(file, _)| vec![0; usize::try_from(file.size()).unwrap_or_default()])
        .collect();
    for (chunk, link, places) in fetches {
        let raw = remote::fetch(client, Some(link), MAX_CHUNK_FILE).await?;
        let verified =
            chunk::decode(&raw, &Want::of(chunk)).map_err(|fault| SaveError::Corrupt(format!("a chunk is {fault}")))?;
        for place in places {
            let source = verified
                .data
                .get(place.offset..place.offset.saturating_add(place.size))
                .ok_or_else(|| SaveError::Corrupt("a file part reads past its chunk".to_owned()))?;
            bodies
                .get_mut(place.file)
                .and_then(|body| body.get_mut(place.at..place.at.saturating_add(place.size)))
                .ok_or_else(|| SaveError::Corrupt("a file part runs past its file".to_owned()))?
                .copy_from_slice(source);
        }
    }
    for ((file, _), body) in plan.files.iter().zip(&bodies) {
        if Sha1Digest::of(body) != *file.sha1() {
            return Err(SaveError::Corrupt(format!(
                "{} does not match its SHA-1",
                Shown(file.path().as_str())
            )));
        }
    }
    Ok(bodies)
}

/// A remote or manifest path as a log line or error may show it: at most 120 characters.
struct Shown<'text>(&'text str);

impl fmt::Display for Shown<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let shown: String = self.0.chars().take(120).collect();
        write!(f, "{shown:?}")
    }
}
