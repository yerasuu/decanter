// SPDX-License-Identifier: GPL-3.0-or-later
//! Installing, verifying and uninstalling an Amazon game (kb #1613, stage S4 of
//! docs/amazon-nile-integration-analysis.md, sections 4.1 to 4.3).
//!
//! Shaped as `decanter-gog`'s installs: files are fetched into a staging folder beside the library's
//! `common/`, with a log of the finished ones so a pause, a lost network or a crash carries on from
//! them, under the download slot every store shares; only once every file is in and verified do they
//! move into `common/<title>`, and the install is recorded as an [`InstalledGame`] with the raw
//! manifest kept beside it to plan the next update, verify and uninstall without asking Amazon.
//!
//! ```text
//! <root>/decanter/amazon/downloading/<account>/<product>/
//!   lock            held while a run owns the download
//!   progress.log    one record per finished file
//!   files/<path>    a finished file; `<path>.part` one in flight
//! <root>/decanter/amazon/manifests/<product>.raw
//! ```

mod disk;
mod distribution;
mod fetch;
mod fuel;
mod import;
mod sdk;
mod stop;

use core::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use core::time::Duration;
use std::collections::HashSet;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use anyhow::Context as _;
use decanter_core::{GameArt, InstalledGame, Library, Storefront};
use decanter_runtime::install::{Cancelled, PauseReason, Paused, Progress, RateTracker, Room};
use decanter_runtime::journal::{LineLog, LockError, StagingLock};
use tokio::sync::Mutex as AsyncMutex;

use self::distribution::{Asked, GameDownload, ManifestFetch};
use self::fetch::{Failure, Job, Run};
use self::stop::{Stop, StopFlag};
use crate::library::is_id;
use crate::manifest::{Manifest, ManifestPath, parse_signed_manifest, plan};

pub use fuel::{FuelLaunch, read_fuel};
pub use import::{Differences, Imported, import};
pub use sdk::{SdkDirs, ensure_sdk};
pub use stop::{cancel, pause, pause_for_quit};

const LOG: &str = "progress.log";
const FILES: &str = "files";
const LOG_FIRST: &[u8] = b"amazon staging 1";

/// Kept free beyond what a download still needs, at least, as for GOG's.
const MIN_MARGIN: u64 = 1024 * 1024 * 1024;

/// A run this one takes over from lets go within one read; past this, another process owns it.
const LOCK_WAIT: Duration = Duration::from_secs(15);

/// One file that does not match the saved manifest.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct BrokenFile {
    /// `/`-separated, relative to the game's folder.
    pub path: String,
    pub reason: BrokenReason,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum BrokenReason {
    Missing,
    /// Another size or hash, or a symbolic link where the game has a file.
    Mismatched,
}

/// What verifying an installed game found.
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct VerifyReport {
    pub files_checked: usize,
    pub broken: Vec<BrokenFile>,
}

/// What a run is for: a repair hashes every file of the game, an install or update only those the
/// saved manifest says changed or that are not there.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Install,
    Repair,
}

/// The game a run is about, as the caller names it.
#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
pub struct Target<'target> {
    pub account_id: &'target str,
    /// The game's id: its record, its folders and its saved manifest are keyed by it.
    pub product_id: &'target str,
    /// What `GetGameDownload` takes; the library listing keeps it beside the product.
    pub entitlement_id: &'target str,
    /// The title the folder under `common/` is named after, on a first install.
    pub title: &'target str,
}

impl<'target> Target<'target> {
    #[inline]
    #[must_use]
    pub const fn new(
        account_id: &'target str,
        product_id: &'target str,
        entitlement_id: &'target str,
        title: &'target str,
    ) -> Self {
        Self {
            account_id,
            product_id,
            entitlement_id,
            title,
        }
    }
}

/// Installs `target`'s current version, or updates it to that version, reporting progress through
/// `on_progress`. Running it again after a pause, a lost network or a crash carries on from the
/// files that were finished; files already in the game's folder with the right hash are kept.
///
/// # Errors
///
/// [`Cancelled`] after [`cancel`], which deletes what was fetched; [`Paused`] after [`pause`], a
/// quit, a full disk or a network that stayed down, keeping it; anything else is a failure: a closed
/// session, a manifest that fails its checks, a file that kept arriving corrupted, a disk error.
#[inline]
pub async fn install(
    library: &Library,
    target: Target<'_>,
    on_progress: impl FnMut(Progress) + Send + 'static,
) -> anyhow::Result<()> {
    run(library, target, Kind::Install, on_progress).await
}

/// Hashes every file of the installed `target` and fetches again what is missing or wrong, from
/// the current version (an update, when Amazon has published one since).
///
/// # Errors
///
/// As [`install`]; also when the game is not installed.
#[inline]
pub async fn repair(
    library: &Library,
    target: Target<'_>,
    on_progress: impl FnMut(Progress) + Send + 'static,
) -> anyhow::Result<()> {
    run(library, target, Kind::Repair, on_progress).await
}

async fn run(
    library: &Library,
    target: Target<'_>,
    kind: Kind,
    on_progress: impl FnMut(Progress) + Send + 'static,
) -> anyhow::Result<()> {
    let Target {
        account_id,
        product_id,
        entitlement_id,
        ..
    } = target;
    anyhow::ensure!(
        is_id(product_id) && is_id(entitlement_id),
        "the Amazon product or entitlement id is not usable"
    );
    let registration = stop::register(account_id, product_id);
    let flag = &registration.flag;
    let installed = library.read_manifest(account_id, Storefront::AmazonGames, product_id);
    anyhow::ensure!(
        kind == Kind::Install || installed.is_some(),
        "the Amazon game is not installed, so there is nothing to repair"
    );
    let (dest, install_dir) = destination(library, target, installed.as_ref())?;
    let root = dest
        .parent()
        .and_then(Path::parent)
        .with_context(|| format!("{} is not under a library's common/", dest.display()))?
        .to_path_buf();
    let staging = disk::staging_dir(&root, account_id, product_id)?;
    tokio::fs::create_dir_all(&staging)
        .await
        .with_context(|| format!("creating {}", staging.display()))?;
    let held = lock_staging(&staging).await?;
    tracing::info!(account_id, product_id, ?kind, dest = %dest.display(), "amazon download started");

    let slot = tokio::select! {
        slot = decanter_runtime::install::take_slot(library) => slot,
        () = flag.stopped() => return Err(ended(flag, &staging, &root, 0).await),
    };
    let (download, raw) = locate(flag, target, &staging, &root).await?;
    let manifest = Arc::new(parse_signed_manifest(&raw).context("Amazon's manifest for this game was refused")?);
    let saved = saved_manifest(&root, product_id, installed.is_some()).await;
    let (log, done) = open_log(&staging).await?;
    let files_dir = staging.join(FILES);
    let checked = check(
        Arc::clone(&manifest),
        saved.as_ref().map(Arc::clone),
        kind,
        &dest,
        &files_dir,
        done,
    )
    .await?;
    let removed: Vec<ManifestPath> = saved
        .as_deref()
        .map(|old| plan(&manifest, Some(old)).remove.into_iter().cloned().collect())
        .unwrap_or_default();
    tracing::info!(
        account_id,
        product_id,
        version_id = download.version_id,
        files = manifest.files().len(),
        fetch = checked.fetch.len(),
        staged = checked.staged.len(),
        present = checked.present,
        removed = removed.len(),
        "amazon download planned"
    );
    let transfer = Transfer {
        download: &download,
        manifest: &manifest,
        checked: &checked,
        flag,
        staging: &staging,
        root: &root,
    };
    transfer.fetch(log, on_progress).await?;
    drop(slot);

    let moved: Vec<usize> = checked.staged.iter().chain(&checked.fetch).copied().collect();
    let landing = {
        let manifest = Arc::clone(&manifest);
        let dest = dest.clone();
        tokio::task::spawn_blocking(move || land(&manifest, &moved, &removed, &dest, &files_dir))
    };
    landing
        .await
        .context("the move into the library stopped")?
        .with_context(|| format!("moving the game into {}", dest.display()))?;
    let previous_program = installed.as_ref().and_then(|game| game.launch_executable.as_deref());
    let launch_executable = launch_program(&dest, previous_program).await;
    let record = match installed {
        Some(game) => InstalledGame {
            build_id: download.version_id.clone(),
            size_on_disk: Some(manifest.total_size()),
            launch_executable,
            ..game
        },
        None => InstalledGame {
            launch_executable,
            ..new_record(target, install_dir, &download, manifest.total_size())
        },
    };
    save_record(library, &root, &record, &raw).await?;
    drop(held);
    if let Err(err) = tokio::fs::remove_dir_all(&staging).await {
        tracing::warn!(staging = %staging.display(), error = %err, "couldn't remove the finished amazon staging");
    }
    tracing::info!(
        account_id,
        product_id,
        version_id = download.version_id,
        ?kind,
        "amazon download finished"
    );
    Ok(())
}

/// Asks Amazon for the current version and fetches its manifest, raw. A network that is down
/// pauses the download, so it is carried on later without a word from the user.
async fn locate(
    flag: &StopFlag,
    target: Target<'_>,
    staging: &Path,
    root: &Path,
) -> anyhow::Result<(GameDownload, Vec<u8>)> {
    let Target {
        account_id,
        product_id,
        entitlement_id,
        ..
    } = target;
    let asked = tokio::select! {
        asked = distribution::game_download(account_id, entitlement_id) => asked?,
        () = flag.stopped() => return Err(ended(flag, staging, root, 0).await),
    };
    let download = match asked {
        Asked::Ready(download) => download,
        Asked::SessionClosed => anyhow::bail!("Amazon closed the session: log in to Amazon Games again in Settings"),
        Asked::Unreachable(reason) => {
            tracing::warn!(
                account_id,
                product_id,
                reason,
                "amazon unreachable, the download pauses"
            );
            return Err(paused(PauseReason::Network, 0, root, None));
        }
    };
    let fetched = tokio::select! {
        fetched = distribution::manifest(&download) => fetched,
        () = flag.stopped() => return Err(ended(flag, staging, root, 0).await),
    };
    match fetched {
        Ok(raw) => Ok((download, raw)),
        Err(ManifestFetch::Unreachable(reason)) => {
            tracing::warn!(
                account_id,
                product_id,
                reason,
                "amazon manifest not fetched, the download pauses"
            );
            Err(paused(PauseReason::Network, 0, root, None))
        }
        Err(ManifestFetch::TooLarge) => anyhow::bail!("Amazon's manifest for this game is larger than decanter reads"),
    }
}

/// The fetch phase of one run.
struct Transfer<'run> {
    download: &'run GameDownload,
    manifest: &'run Manifest,
    checked: &'run Checked,
    flag: &'run StopFlag,
    staging: &'run Path,
    root: &'run Path,
}

impl Transfer<'_> {
    /// Fetches what [`check`] found missing into staging, once the disk has room for it.
    async fn fetch(&self, log: LineLog, mut on_progress: impl FnMut(Progress) + Send) -> anyhow::Result<()> {
        let files_dir = self.staging.join(FILES);
        let jobs: Vec<Job<'_>> = self
            .checked
            .fetch
            .iter()
            .filter_map(|&index| {
                let file = self.manifest.files().get(index)?;
                Some(Job {
                    index,
                    file,
                    done: files_dir.join(file.path().as_path()),
                })
            })
            .collect();
        let staged_bytes = self.checked.staged_bytes(self.manifest);
        let total = jobs
            .iter()
            .fold(staged_bytes, |sum, job| sum.saturating_add(job.file.size()));
        let on_disk: Vec<AtomicU64> = jobs.iter().map(|job| AtomicU64::new(part_len(&job.part()))).collect();
        let bytes_done = || {
            on_disk.iter().fold(staged_bytes, |sum, bytes| {
                sum.saturating_add(bytes.load(Ordering::Relaxed))
            })
        };
        if let Some(room) = lacking_room(self.staging, total, bytes_done()) {
            tracing::warn!(
                needed = room.needed,
                free = room.free,
                "not enough free space for the amazon download"
            );
            return Err(paused(PauseReason::DiskFull, bytes_done(), self.root, Some(room)));
        }
        let current = AtomicUsize::new(usize::MAX);
        let log = AsyncMutex::new(log);
        let fetch_run = Run {
            download: self.download,
            flag: self.flag,
            log: &log,
            on_disk: &on_disk,
            current: &current,
        };
        let mut rate = RateTracker::new(bytes_done());
        let fetched = fetch::fetch_all(&fetch_run, &jobs, || {
            let downloaded = bytes_done();
            let current_file = self
                .manifest
                .files()
                .get(current.load(Ordering::Relaxed))
                .map(|file| file.path().as_str().to_owned())
                .unwrap_or_default();
            on_progress(Progress {
                current_file,
                bytes_downloaded: downloaded,
                total_bytes: Some(total),
                bytes_per_second: rate.sample(downloaded),
                build_switch: None,
            });
        })
        .await;
        match fetched {
            Ok(()) => Ok(()),
            Err(Failure::Stopped) if self.flag.requested() != Some(Stop::Abort) => {
                Err(ended(self.flag, self.staging, self.root, bytes_done()).await)
            }
            Err(Failure::Unreachable) => Err(paused(PauseReason::Network, bytes_done(), self.root, None)),
            Err(Failure::DiskFull) => {
                let room = lacking_room(self.staging, total, bytes_done());
                Err(paused(PauseReason::DiskFull, bytes_done(), self.root, room))
            }
            Err(Failure::Stopped) => anyhow::bail!("the amazon download stopped for a failure it did not report"),
            Err(Failure::Corrupt { path }) => anyhow::bail!("{path} kept arriving from Amazon with the wrong hash"),
            Err(Failure::Io { path, source }) => {
                Err(anyhow::Error::new(source).context(format!("writing {path} into staging")))
            }
        }
    }
}

/// The program the record starts: the one it had while that is still in the game's folder, so a
/// program picked in the properties survives an update, else the one `fuel.json` names. A game whose
/// `fuel.json` can't be read installs all the same, without one, which the library says.
async fn launch_program(dest: &Path, previous: Option<&str>) -> Option<String> {
    let kept = previous.filter(|program| {
        decanter_runtime::sanitize_relative_path(program).is_some_and(|relative| {
            std::fs::symlink_metadata(dest.join(relative)).is_ok_and(|meta| meta.file_type().is_file())
        })
    });
    if let Some(program) = kept {
        return Some(program.to_owned());
    }
    let dest = dest.to_path_buf();
    let read = tokio::task::spawn_blocking(move || fuel::read_fuel(&dest)).await;
    match read {
        Ok(Ok(fuel)) => Some(fuel.command.as_str().to_owned()),
        Ok(Err(err)) => {
            tracing::warn!(error = %format_args!("{err:#}"), "no program from the amazon game's fuel.json");
            None
        }
        Err(err) => {
            tracing::warn!(error = %err, "reading the amazon game's fuel.json stopped");
            None
        }
    }
}

/// Saves the raw manifest, then the install record: a record never names a manifest that is not
/// there to uninstall by.
async fn save_record(library: &Library, root: &Path, record: &InstalledGame, raw: &[u8]) -> anyhow::Result<()> {
    let saved_path = disk::saved_manifest(root, &record.game_id)?;
    if let Some(parent) = saved_path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    decanter_runtime::journal::write_atomic(&saved_path, raw)
        .await
        .with_context(|| format!("saving the manifest to {}", saved_path.display()))?;
    library
        .clone()
        .targeting(root)
        .write_manifest(record)
        .context("writing the install record")
}

/// The game's folder under `common/` and its name: the installed one, else one named after the
/// title, with the store added when another game already uses that name.
fn destination(
    library: &Library,
    target: Target<'_>,
    installed: Option<&InstalledGame>,
) -> anyhow::Result<(PathBuf, String)> {
    if let Some(game) = installed {
        let dest = library
            .install_dir(&game.install_dir)
            .with_context(|| format!("the folder {:?} of the installed game is unusable", game.install_dir))?;
        return Ok((dest, game.install_dir.clone()));
    }
    let library = staged_root(library, target.account_id, target.product_id)
        .map_or_else(|| library.clone(), |root| library.clone().targeting(&root));
    let mut name = disk::install_dir_name(target.title, target.product_id);
    if library.install_dir_is_shared(&name, target.account_id, target.product_id) {
        name = format!("{name} (Amazon Games)");
    }
    let dest = library
        .install_dir(&name)
        .with_context(|| format!("the folder {name:?} belongs to a Steam game"))?;
    Ok((dest, name))
}

/// The library folder holding a stopped download of the game, so carrying it on stays on its disk.
fn staged_root(library: &Library, account_id: &str, product_id: &str) -> Option<PathBuf> {
    library
        .roots()
        .into_iter()
        .find(|root| disk::staging_dir(root, account_id, product_id).is_ok_and(|staging| staging.join(LOG).is_file()))
}

/// Locks the staging folder, waiting for a run this one replaced to let go of it.
async fn lock_staging(staging: &Path) -> anyhow::Result<StagingLock> {
    let deadline = Instant::now() + LOCK_WAIT;
    loop {
        match decanter_runtime::journal::lock(staging) {
            Ok(held) => return Ok(held),
            Err(LockError::Busy(_)) if Instant::now() < deadline => {
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
            Err(err) => {
                return Err(anyhow::Error::new(err).context("another decanter process is downloading this game"));
            }
        }
    }
}

/// The manifest saved at the last install, when there is one that still reads.
async fn saved_manifest(root: &Path, product_id: &str, is_installed: bool) -> Option<Arc<Manifest>> {
    if !is_installed {
        return None;
    }
    let path = disk::saved_manifest(root, product_id).ok()?;
    match read_saved(&path).await {
        Ok(manifest) => Some(Arc::new(manifest)),
        Err(err) => {
            tracing::warn!(path = %path.display(), error = %format_args!("{err:#}"), "the saved amazon manifest is unusable, every file is checked against the new one");
            None
        }
    }
}

async fn read_saved(path: &Path) -> anyhow::Result<Manifest> {
    let raw = tokio::fs::read(path)
        .await
        .with_context(|| format!("reading {}", path.display()))?;
    parse_signed_manifest(&raw).context("the saved manifest was refused")
}

/// Opens the staging's log, or starts one, and returns the finished files it records.
async fn open_log(staging: &Path) -> anyhow::Result<(LineLog, HashSet<String>)> {
    let path = staging.join(LOG);
    if !path.is_file() {
        let log = LineLog::create(&path, LOG_FIRST)
            .await
            .with_context(|| format!("creating {}", path.display()))?;
        return Ok((log, HashSet::new()));
    }
    let (log, records) = LineLog::open(&path)
        .await
        .with_context(|| format!("opening {}", path.display()))?;
    let mut unknown = 0_usize;
    let done = records
        .into_iter()
        .filter(|record| record.as_slice() != LOG_FIRST)
        .filter_map(|record| {
            let text = String::from_utf8(record).ok().filter(|text| text.starts_with("done "));
            if text.is_none() {
                unknown = unknown.saturating_add(1);
            }
            text
        })
        .collect();
    if unknown > 0 {
        tracing::warn!(path = %path.display(), unknown, "skipped records the amazon download log does not know");
    }
    Ok((log, done))
}

/// Where each file of the manifest stands before the download.
struct Checked {
    /// Indices of files to fetch.
    fetch: Vec<usize>,
    /// Indices of files already finished in staging.
    staged: Vec<usize>,
    /// Files already in the game's folder as the manifest wants them.
    present: usize,
}

impl Checked {
    fn staged_bytes(&self, manifest: &Manifest) -> u64 {
        self.staged
            .iter()
            .filter_map(|&index| manifest.files().get(index))
            .fold(0, |sum, file| sum.saturating_add(file.size()))
    }
}

/// Sorts every file into fetched, staged or present. A file the saved manifest already had is
/// taken as present when its size is right, except in a repair; any other present file is hashed.
async fn check(
    manifest: Arc<Manifest>,
    saved: Option<Arc<Manifest>>,
    kind: Kind,
    dest: &Path,
    files_dir: &Path,
    done: HashSet<String>,
) -> anyhow::Result<Checked> {
    let (dest, files_dir) = (dest.to_path_buf(), files_dir.to_path_buf());
    tokio::task::spawn_blocking(move || {
        let changed: Option<HashSet<&str>> = saved.as_deref().map(|old| {
            plan(&manifest, Some(old))
                .fetch
                .iter()
                .map(|file| file.path().as_str())
                .collect()
        });
        let mut checked = Checked {
            fetch: Vec::new(),
            staged: Vec::new(),
            present: 0,
        };
        for (index, file) in manifest.files().iter().enumerate() {
            let staged = disk::inside(&files_dir, file.path())?;
            let is_staged = if done.contains(&fetch::done_record(file)) {
                disk::has_size(&staged, file.size())?
            } else {
                // Renamed into staging, but a crash came before its record.
                disk::matches(&staged, file)?
            };
            if is_staged {
                checked.staged.push(index);
                continue;
            }
            let installed = disk::inside(&dest, file.path())?;
            let is_unchanged = kind == Kind::Install
                && changed
                    .as_ref()
                    .is_some_and(|changed| !changed.contains(file.path().as_str()));
            let is_present = if is_unchanged {
                disk::has_size(&installed, file.size())?
            } else {
                disk::matches(&installed, file)?
            };
            if is_present {
                checked.present = checked.present.saturating_add(1);
            } else {
                checked.fetch.push(index);
            }
        }
        Ok::<_, io::Error>(checked)
    })
    .await
    .context("checking the files on disk stopped")?
    .context("checking the files already on disk")
}

fn part_len(part: &Path) -> u64 {
    std::fs::symlink_metadata(part)
        .ok()
        .filter(|meta| meta.file_type().is_file())
        .map_or(0, |meta| meta.len())
}

/// The room the download lacks on `staging`'s disk: what is left of it, plus the larger of
/// [`MIN_MARGIN`] and 2 % of it, as for GOG's (#1277). `None` when there is room, or when the free
/// space can't be read.
fn lacking_room(staging: &Path, total: u64, done: u64) -> Option<Room> {
    let (free, _) = decanter_core::disk_usage(staging)?;
    let margin = total.div_ceil(50).max(MIN_MARGIN);
    let room = Room::new(total.saturating_sub(done).saturating_add(margin), free);
    (room.short() > 0).then_some(room)
}

/// Deletes the files the new version dropped, then moves every finished file into the game's
/// folder; deleting first, since a file renamed only in case would otherwise be deleted after it
/// lands on a case-insensitive disk.
fn land(
    manifest: &Manifest,
    moved: &[usize],
    removed: &[ManifestPath],
    dest: &Path,
    files_dir: &Path,
) -> anyhow::Result<()> {
    std::fs::create_dir_all(dest).with_context(|| format!("creating {}", dest.display()))?;
    if !removed.is_empty() {
        let removal = disk::remove_files(dest, removed);
        let pruned = disk::prune_dirs(dest, removed, &[]);
        tracing::info!(
            removed = removal.removed,
            missing = removal.missing,
            failed = removal.failed,
            pruned,
            "deleted the files the new amazon version dropped"
        );
    }
    for file in moved.iter().filter_map(|&index| manifest.files().get(index)) {
        let staged = disk::inside(files_dir, file.path())?;
        let destination = disk::inside(dest, file.path())?;
        disk::move_into_place(&staged, &destination)
            .with_context(|| format!("moving {} into the game's folder", file.path().as_str()))?;
    }
    disk::create_dirs(dest, manifest).context("creating the game's empty folders")?;
    tracing::debug!(files = moved.len(), "moved the amazon download into the library");
    Ok(())
}

fn new_record(target: Target<'_>, install_dir: String, download: &GameDownload, size: u64) -> InstalledGame {
    InstalledGame {
        account_id: target.account_id.to_owned(),
        storefront: Storefront::AmazonGames,
        game_id: target.product_id.to_owned(),
        name: target.title.to_owned(),
        install_dir,
        build_id: download.version_id.clone(),
        size_on_disk: Some(size),
        size_fingerprint: None,
        launch_executable: None,
        last_played: None,
        proton: None,
        dlcs: std::collections::BTreeMap::new(),
        branch: None,
        language: None,
        cloud_save_path: None,
        cloud_save_last_sync: None,
        art: GameArt::default(),
        umu_id: None,
        umu_store: None,
        prefix_of: None,
    }
}

fn paused(reason: PauseReason, bytes_done: u64, root: &Path, room: Option<Room>) -> anyhow::Error {
    Paused {
        reason,
        bytes_done,
        root: Some(root.to_path_buf()),
        room,
    }
    .into()
}

/// What a stopped run fails with: a cancel deletes the staging, anything else keeps it.
async fn ended(flag: &StopFlag, staging: &Path, root: &Path, bytes_done: u64) -> anyhow::Error {
    match flag.requested() {
        Some(Stop::Cancel) => {
            if let Err(err) = tokio::fs::remove_dir_all(staging).await
                && err.kind() != io::ErrorKind::NotFound
            {
                tracing::warn!(staging = %staging.display(), error = %err, "couldn't delete a cancelled amazon download");
            }
            Cancelled.into()
        }
        Some(Stop::Quit) => paused(PauseReason::AppClosed, bytes_done, root, None),
        Some(Stop::Pause | Stop::Abort) | None => paused(PauseReason::User, bytes_done, root, None),
    }
}

/// Deletes a stopped download of the game in every library folder.
///
/// # Errors
///
/// When a run still holds it, or it can't be deleted.
#[inline]
pub async fn discard(library: &Library, account_id: &str, product_id: &str) -> anyhow::Result<()> {
    for root in library.roots() {
        let staging = disk::staging_dir(&root, account_id, product_id)?;
        if !staging.is_dir() {
            continue;
        }
        let held = decanter_runtime::journal::lock(&staging).context("the amazon download is still running")?;
        tokio::fs::remove_dir_all(&staging)
            .await
            .with_context(|| format!("deleting {}", staging.display()))?;
        drop(held);
        tracing::info!(account_id, product_id, staging = %staging.display(), "discarded a stopped amazon download");
    }
    Ok(())
}

/// The installed game's folder and its saved manifest.
fn installed_files(
    library: &Library,
    account_id: &str,
    product_id: &str,
) -> anyhow::Result<Option<(InstalledGame, PathBuf, PathBuf)>> {
    let Some(game) = library.read_manifest(account_id, Storefront::AmazonGames, product_id) else {
        return Ok(None);
    };
    let dest = library
        .install_dir(&game.install_dir)
        .with_context(|| format!("the folder {:?} of the installed game is unusable", game.install_dir))?;
    let root = library
        .root_of(account_id, product_id)
        .context("the install record's library folder is gone")?;
    let saved = disk::saved_manifest(&root, product_id)?;
    Ok(Some((game, dest, saved)))
}

/// Hashes every file of `manifest` under `dest`.
fn hash_files(manifest: &Manifest, dest: &Path) -> VerifyReport {
    let mut report = VerifyReport {
        files_checked: manifest.files().len(),
        broken: Vec::new(),
    };
    for file in manifest.files() {
        // A folder on the way that is a link, or a file that can't be read, is as broken as a
        // wrong hash: a repair puts the game's own file there.
        let reason = disk::inside(dest, file.path()).map_or(Some(BrokenReason::Mismatched), |path| {
            match std::fs::symlink_metadata(&path) {
                Err(err) if err.kind() == io::ErrorKind::NotFound => Some(BrokenReason::Missing),
                _ => (!disk::matches(&path, file).unwrap_or(false)).then_some(BrokenReason::Mismatched),
            }
        });
        if let Some(reason) = reason {
            report.broken.push(BrokenFile {
                path: file.path().as_str().to_owned(),
                reason,
            });
        }
    }
    report
}

/// Hashes every file of the installed game against the manifest it was installed from, without
/// asking Amazon.
///
/// # Errors
///
/// When the game is not installed, or its saved manifest is missing or refused.
#[inline]
pub async fn verify(library: &Library, account_id: &str, product_id: &str) -> anyhow::Result<VerifyReport> {
    let (_, dest, saved) =
        installed_files(library, account_id, product_id)?.context("the Amazon game is not installed")?;
    let manifest = read_saved(&saved).await?;
    let report = tokio::task::spawn_blocking(move || hash_files(&manifest, &dest))
        .await
        .context("verifying the files stopped")?;
    tracing::info!(
        account_id,
        product_id,
        files_checked = report.files_checked,
        broken = report.broken.len(),
        "verified an amazon game"
    );
    Ok(report)
}

/// Uninstalls the game: the files its manifest lists, then the folders they leave empty, then the
/// game's folder if nothing else is in it. What the user or the game added (saves, mods) stays. A
/// folder another install shares keeps its files, and a missing saved manifest leaves them all,
/// since decanter then can't tell its files from anyone else's.
///
/// # Errors
///
/// When files can't be deleted, which keeps the record for another try, or the record can't be
/// removed.
#[inline]
pub async fn uninstall(library: &Library, account_id: &str, product_id: &str) -> anyhow::Result<()> {
    discard(library, account_id, product_id).await?;
    let Some((game, dest, saved)) = installed_files(library, account_id, product_id)? else {
        tracing::info!(
            account_id,
            product_id,
            "amazon game not installed, nothing to uninstall"
        );
        return Ok(());
    };
    if library.install_dir_is_shared(&game.install_dir, account_id, product_id) {
        tracing::info!(account_id, product_id, dest = %dest.display(), "amazon game folder shared with another install, files kept");
    } else {
        match read_saved(&saved).await {
            Ok(manifest) => {
                let folder = dest.clone();
                let removal = tokio::task::spawn_blocking(move || {
                    let paths = manifest.files().iter().map(crate::manifest::ManifestFile::path);
                    let removal = disk::remove_files(&folder, paths.clone());
                    let pruned = disk::prune_dirs(&folder, paths, manifest.dirs());
                    let is_folder_gone = std::fs::remove_dir(&folder).is_ok();
                    (removal, pruned, is_folder_gone)
                })
                .await
                .context("deleting the game's files stopped")?;
                let (removal, pruned, is_folder_gone) = removal;
                tracing::info!(
                    account_id,
                    product_id,
                    removed = removal.removed,
                    missing = removal.missing,
                    failed = removal.failed,
                    pruned,
                    is_folder_gone,
                    "deleted an amazon game's files"
                );
                anyhow::ensure!(
                    removal.failed == 0,
                    "{} of the game's files could not be deleted from {}",
                    removal.failed,
                    dest.display()
                );
            }
            Err(err) => tracing::warn!(
                account_id,
                product_id,
                dest = %dest.display(),
                error = %format_args!("{err:#}"),
                "no usable saved amazon manifest, the game's files are left in place"
            ),
        }
    }
    library
        .remove_manifest(account_id, Storefront::AmazonGames, product_id)
        .context("removing the install record")?;
    if let Err(err) = tokio::fs::remove_file(&saved).await
        && err.kind() != io::ErrorKind::NotFound
    {
        tracing::warn!(path = %saved.display(), error = %err, "couldn't delete the saved amazon manifest");
    }
    tracing::info!(account_id, product_id, "amazon game uninstalled");
    Ok(())
}
