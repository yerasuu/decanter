// SPDX-License-Identifier: GPL-3.0-or-later
//! Installing, verifying and uninstalling an Epic game (kb #1619, stage E5), shaped as Amazon's:
//! chunks are fetched and the files assembled in a staging folder beside the library's `common/`,
//! with a log of what is done so a pause, a lost network or a crash carries on from it, under the
//! download slot every store shares; only once every file is in and its SHA-1 checked do they move
//! into `common/<title>`, and the install is recorded as an [`InstalledGame`] with the manifest kept
//! beside it to plan the next update, verify and uninstall without asking Epic.
//!
//! ```text
//! <root>/decanter/epic/downloading/<account>/<app name>/
//!   lock            held while a run owns the download
//!   download.json   title, kind, size and how it stopped, to list it after a restart
//!   progress.log    the build's manifest, the chunks written and the files finished
//!   files/<path>    a finished file; `<path>.part` one being assembled
//! <root>/decanter/epic/manifests/<app name>.manifest
//! ```
//!
//! Only the base game is installed: DLC, install tags, delta manifests (spec, 5.5), prerequisites
//! and encrypted builds are left for later.

mod chunk;
mod disk;
mod distribution;
mod fetch;

use std::collections::HashSet;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::Context as _;
use decanter_core::{GameArt, InstalledGame, Library, Storefront};
use decanter_runtime::install::staging::{Layout, Record};
use decanter_runtime::install::stop::{Downloads, Stop, StopFlag};
pub use decanter_runtime::install::{BrokenFile, BrokenReason, VerifyReport};
use decanter_runtime::install::{
    Cancelled, ManifestPath, PauseReason, Paused, Progress, RateTracker, Room, UnfinishedDownload, lacking_room,
};
use decanter_runtime::journal::{LineLog, StagingLock};

use self::distribution::{Asked, GameDownload, ManifestFetch};
use self::fetch::{Failure, Job, Run};
use crate::library::is_id;
use crate::manifest::{self, Guid, Manifest, ManifestError, Sha1Digest, parse_manifest};

const LOG: &str = "progress.log";
const FILES: &str = "files";
const LOG_FIRST: &[u8] = b"epic staging 1";
const ID_MAX_LEN: usize = 128;

static DOWNLOADS: Downloads = Downloads::new("epic");

static LAYOUT: Layout = Layout {
    storefront: Storefront::EpicGames,
    store: "epic",
    downloading: disk::downloading_dir,
    is_id: is_app_name,
    log: LOG,
    files: FILES,
};

/// Cancels the Epic download of `(account_id, app_name)`, which then deletes what it fetched; a
/// paused one, which nothing runs, is deleted with [`discard`].
#[inline]
pub fn cancel(account_id: &str, app_name: &str) {
    DOWNLOADS.cancel(account_id, app_name);
}

/// Pauses the Epic download of `(account_id, app_name)`, keeping what it fetched.
#[inline]
pub fn pause(account_id: &str, app_name: &str) {
    DOWNLOADS.pause(account_id, app_name);
}

/// Pauses every Epic download because decanter is quitting, waiting up to `wait` for each to
/// record it; whether all of them did.
#[inline]
pub async fn pause_for_quit(wait: core::time::Duration) -> bool {
    DOWNLOADS.pause_for_quit(wait).await
}

/// Every unfinished Epic download in `library`'s folders, for the app to list when it starts.
#[inline]
pub async fn unfinished_downloads(library: &Library) -> Vec<UnfinishedDownload> {
    decanter_runtime::install::staging::unfinished_downloads(library, &LAYOUT).await
}

/// An `appName` that can key an install record and name its folders: the record's file name takes
/// only these characters, so one with a `:` that the library listing allows is refused here.
fn is_app_name(app_name: &str) -> bool {
    (1..=ID_MAX_LEN).contains(&app_name.len())
        && app_name.bytes().next().is_some_and(|byte| byte.is_ascii_alphanumeric())
        && app_name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
}

/// What a run is for: a repair hashes every file of the game, an install or update only those the
/// saved manifest says changed or that are not there.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Install,
    Repair,
}

/// The game a run is about, as the library listing cached it.
#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
pub struct Target<'target> {
    pub account_id: &'target str,
    /// The game's id: its record, its folders and its saved manifest are keyed by it.
    pub app_name: &'target str,
    /// The sandbox and catalog item the manifest request names beside it (spec, 5.1).
    pub namespace: &'target str,
    pub catalog_item_id: &'target str,
    /// The title the folder under `common/` is named after, on a first install.
    pub title: &'target str,
}

impl<'target> Target<'target> {
    #[inline]
    #[must_use]
    pub const fn new(
        account_id: &'target str,
        app_name: &'target str,
        namespace: &'target str,
        catalog_item_id: &'target str,
        title: &'target str,
    ) -> Self {
        Self {
            account_id,
            app_name,
            namespace,
            catalog_item_id,
            title,
        }
    }
}

/// Installs `target`'s current build, or updates it to that build, reporting progress through
/// `on_progress`. Running it again after a pause, a lost network or a crash carries on from what
/// was done; files already in the game's folder with the right SHA-1 are kept.
///
/// # Errors
///
/// [`Cancelled`] after [`cancel`], which deletes what was fetched; [`Paused`] after [`pause`], a
/// quit, a full disk or a network that stayed down, keeping it; anything else is a failure: a closed
/// session, a manifest that fails its checks, a chunk or file that kept arriving corrupted, a disk
/// error.
#[inline]
pub async fn install(
    library: &Library,
    target: Target<'_>,
    on_progress: impl FnMut(Progress) + Send + 'static,
) -> anyhow::Result<()> {
    run(library, target, Kind::Install, on_progress).await
}

/// Hashes every file of the installed `target` and fetches again what is missing or wrong, from
/// the current build (an update, when Epic has published one since).
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
        account_id, app_name, ..
    } = target;
    anyhow::ensure!(
        is_app_name(app_name) && is_id(target.namespace) && is_id(target.catalog_item_id),
        "the Epic game's ids are not usable"
    );
    let registration = DOWNLOADS.register(account_id, app_name);
    let flag = registration.flag();
    let installed = library.read_manifest(account_id, Storefront::EpicGames, app_name);
    anyhow::ensure!(
        kind == Kind::Install || installed.is_some(),
        "the Epic game is not installed, so there is nothing to repair"
    );
    let (dest, install_dir) = destination(library, target, installed.as_ref())?;
    let root = dest
        .parent()
        .and_then(Path::parent)
        .with_context(|| format!("{} is not under a library's common/", dest.display()))?
        .to_path_buf();
    let staging = disk::staging_dir(&root, account_id, app_name)?;
    tokio::fs::create_dir_all(&staging)
        .await
        .with_context(|| format!("creating {}", staging.display()))?;
    let held = lock_staging(&staging).await?;
    tracing::info!(account_id, app_name, ?kind, dest = %dest.display(), "epic download started");
    let mut staged = Record::new(target.title, kind == Kind::Repair);
    staged.save(&staging).await;

    let landed: anyhow::Result<String> = async {
        // Biased, so a stop already asked for ends it here, before Epic is asked anything.
        let slot = tokio::select! {
            biased;
            () = flag.stopped() => return Err(ended(flag, &staging, &root, 0).await),
            slot = decanter_runtime::install::take_slot(library) => slot,
        };
        let (download, raw) = locate(flag, target, &staging, &root).await?;
        let (manifest, manifest_id, raw) = read_downloaded(raw).await?;
        if manifest.app_name() != app_name {
            tracing::info!(
                app_name,
                manifest_app = manifest.app_name(),
                "epic manifest names the game otherwise"
            );
        }
        let saved = saved_manifest(&root, app_name, installed.is_some()).await;
        let journal = open_log(&staging, &manifest_id).await?;
        let files_dir = staging.join(FILES);
        let checked = check(
            Arc::clone(&manifest),
            saved.as_ref().map(Arc::clone),
            kind,
            &dest,
            &files_dir,
            journal.done.clone(),
        )
        .await?;
        staged.planned(checked.total_bytes(&manifest));
        staged.save(&staging).await;
        let removed: Vec<ManifestPath> = saved
            .as_deref()
            .map(|old| manifest::dropped(&manifest, old).into_iter().cloned().collect())
            .unwrap_or_default();
        tracing::info!(
            account_id,
            app_name,
            build_version = manifest.build_version(),
            feature_level = manifest.feature_level(),
            files = manifest.files().len(),
            chunks = manifest.chunks().len(),
            fetch = checked.fetch.len(),
            staged = checked.staged.len(),
            present = checked.present,
            removed = removed.len(),
            is_same_build = journal.is_same_build,
            cdns = ?download.hosts(),
            "epic download planned"
        );
        let transfer = Transfer {
            download: &download,
            manifest: &manifest,
            checked: &checked,
            flag,
            staging: &staging,
            root: &root,
        };
        transfer.fetch(journal, on_progress).await?;
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
        let launch_executable = launch_program(&dest, previous_program, manifest.launch_exe()).await;
        let build_version = manifest.build_version().to_owned();
        let record = match installed {
            Some(game) => InstalledGame {
                build_id: build_version.clone(),
                size_on_disk: Some(manifest.total_size()),
                launch_executable,
                ..game
            },
            None => InstalledGame {
                launch_executable,
                ..new_record(target, install_dir, &build_version, manifest.total_size())
            },
        };
        save_record(library, &root, &record, &raw).await?;
        Ok(build_version)
    }
    .await;
    let build_version = match landed {
        Ok(build_version) => build_version,
        Err(err) => {
            if let Some(paused) = err.downcast_ref::<Paused>() {
                staged.stopped(paused);
                staged.save(&staging).await;
            }
            return Err(err);
        }
    };
    drop(held);
    if let Err(err) = tokio::fs::remove_dir_all(&staging).await {
        tracing::warn!(staging = %staging.display(), error = %err, "couldn't remove the finished epic staging");
    }
    tracing::info!(account_id, app_name, build_version, ?kind, "epic download finished");
    Ok(())
}

/// Parses a downloaded manifest off the async threads, since one can be tens of mebibytes, and
/// names its build by the SHA-1 of its bytes, which the log keys its records to.
async fn read_downloaded(raw: Vec<u8>) -> anyhow::Result<(Arc<Manifest>, Sha1Digest, Vec<u8>)> {
    let parsed = tokio::task::spawn_blocking(move || (parse_manifest(&raw), Sha1Digest::of(&raw), raw))
        .await
        .context("reading the manifest stopped")?;
    match parsed {
        (Ok(manifest), id, raw) => Ok((Arc::new(manifest), id, raw)),
        (Err(err), _, _) => {
            tracing::warn!(error = %err, "epic manifest refused");
            Err(anyhow::Error::new(err).context("Epic's manifest for this game was refused"))
        }
    }
}

/// Asks Epic for the current build and fetches its manifest. A network that is down pauses the
/// download, so it is carried on later without a word from the user.
async fn locate(
    flag: &StopFlag,
    target: Target<'_>,
    staging: &Path,
    root: &Path,
) -> anyhow::Result<(GameDownload, Vec<u8>)> {
    let Target {
        account_id, app_name, ..
    } = target;
    let asked = tokio::select! {
        asked = distribution::game_download(&target) => asked?,
        () = flag.stopped() => return Err(ended(flag, staging, root, 0).await),
    };
    let download = match asked {
        Asked::Ready(download) => download,
        Asked::SessionClosed => anyhow::bail!("Epic closed the session: log in to Epic Games again in Settings"),
        Asked::Unreachable(reason) => {
            tracing::warn!(account_id, app_name, reason, "epic unreachable, the download pauses");
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
                app_name,
                reason,
                "epic manifest not fetched, the download pauses"
            );
            Err(paused(PauseReason::Network, 0, root, None))
        }
        Err(ManifestFetch::Mismatch) => {
            anyhow::bail!("every CDN gave Epic's manifest for this game with another SHA-1 than Epic named")
        }
        Err(ManifestFetch::TooLarge) => anyhow::bail!("Epic's manifest for this game is larger than decanter reads"),
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
    async fn fetch(&self, journal: Journal, mut on_progress: impl FnMut(Progress) + Send) -> anyhow::Result<()> {
        let files_dir = self.staging.join(FILES);
        let mut jobs: Vec<Job<'_>> = self
            .checked
            .fetch
            .iter()
            .filter_map(|&index| {
                let file = self.manifest.files().get(index)?;
                Some(Job {
                    file,
                    done: files_dir.join(file.path().as_path()),
                    is_fresh: !journal.is_same_build,
                })
            })
            .collect();
        let staged_bytes = self.checked.staged_bytes(self.manifest);
        let total = self.checked.total_bytes(self.manifest);
        if let Some(room) = lacking_room(self.staging, total, staged_bytes) {
            tracing::warn!(
                needed = room.needed,
                free = room.free,
                "not enough free space for the epic download"
            );
            return Err(paused(PauseReason::DiskFull, staged_bytes, self.root, Some(room)));
        }
        let mut run = Run {
            manifest: self.manifest,
            bases: &self.download.bases,
            flag: self.flag,
            log: journal.log,
            applied: journal.applied,
        };
        let mut rate: Option<RateTracker> = None;
        let mut bytes_done = staged_bytes;
        let fetched = fetch::fetch_all(&mut run, &mut jobs, staged_bytes, |downloaded, current_file| {
            bytes_done = downloaded;
            let rate = rate.get_or_insert_with(|| RateTracker::new(downloaded));
            on_progress(Progress {
                current_file: current_file.to_owned(),
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
                Err(ended(self.flag, self.staging, self.root, bytes_done).await)
            }
            Err(Failure::Unreachable) => Err(paused(PauseReason::Network, bytes_done, self.root, None)),
            Err(Failure::DiskFull) => {
                let room = lacking_room(self.staging, total, bytes_done);
                Err(paused(PauseReason::DiskFull, bytes_done, self.root, room))
            }
            Err(Failure::Stopped) => anyhow::bail!("the epic download stopped for a failure it did not report"),
            Err(Failure::Corrupt(what)) => anyhow::bail!("{what} kept arriving from Epic's CDN corrupted"),
            Err(Failure::Encrypted) => Err(anyhow::Error::new(ManifestError::Encrypted)),
            Err(Failure::Io { path, source }) => {
                Err(anyhow::Error::new(source).context(format!("writing {path} into staging")))
            }
        }
    }
}

/// The program the record starts: the one it had while that is still in the game's folder, so a
/// program picked in the properties survives an update, else the manifest's `launch_exe` with its
/// backslashes turned and a leading slash dropped (spec, 8.2).
async fn launch_program(dest: &Path, previous: Option<&str>, launch_exe: &str) -> Option<String> {
    let dest = dest.to_path_buf();
    let previous = previous.map(str::to_owned);
    let launch_exe = launch_exe.replace('\\', "/");
    let found = tokio::task::spawn_blocking(move || {
        let is_file = |relative: &str| {
            ManifestPath::parse(relative).ok().and_then(|path| {
                disk::inside(&dest, &path)
                    .ok()
                    .filter(|full| std::fs::symlink_metadata(full).is_ok_and(|meta| meta.file_type().is_file()))?;
                Some(path.as_str().to_owned())
            })
        };
        previous
            .as_deref()
            .and_then(is_file)
            .or_else(|| is_file(launch_exe.trim_start_matches('/')))
    })
    .await;
    match found {
        Ok(Some(program)) => Some(program),
        Ok(None) => {
            tracing::warn!("the epic manifest names no program present in the game's folder");
            None
        }
        Err(err) => {
            tracing::warn!(error = %err, "looking for the epic game's program stopped");
            None
        }
    }
}

/// Saves the manifest, then the install record: a record never names a manifest that is not there
/// to uninstall by.
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
    let library = staged_root(library, target.account_id, target.app_name)
        .map_or_else(|| library.clone(), |root| library.clone().targeting(&root));
    let mut name = disk::install_dir_name(target.title, target.app_name);
    if library.install_dir_is_shared(&name, target.account_id, target.app_name) {
        name = format!("{name} (Epic Games)");
    }
    let dest = library
        .install_dir(&name)
        .with_context(|| format!("the folder {name:?} belongs to a Steam game"))?;
    Ok((dest, name))
}

/// The library folder holding a stopped download of the game, so carrying it on stays on its disk.
fn staged_root(library: &Library, account_id: &str, app_name: &str) -> Option<PathBuf> {
    library
        .roots()
        .into_iter()
        .find(|root| disk::staging_dir(root, account_id, app_name).is_ok_and(|staging| staging.join(LOG).is_file()))
}

async fn lock_staging(staging: &Path) -> anyhow::Result<StagingLock> {
    decanter_runtime::install::lock_staging(staging)
        .await
        .context("another decanter process is downloading this game")
}

/// The manifest saved at the last install, when there is one that still reads.
async fn saved_manifest(root: &Path, app_name: &str, is_installed: bool) -> Option<Arc<Manifest>> {
    if !is_installed {
        return None;
    }
    let path = disk::saved_manifest(root, app_name).ok()?;
    match read_saved(&path).await {
        Ok(manifest) => Some(Arc::new(manifest)),
        Err(err) => {
            tracing::warn!(path = %path.display(), error = %format_args!("{err:#}"), "the saved epic manifest is unusable, every file is checked against the new one");
            None
        }
    }
}

/// The installed game's manifest as saved at its install, which names the program to start and its
/// arguments; `None` when the game is not installed.
///
/// # Errors
///
/// The record's library folder is gone, or the saved manifest can't be read or is refused.
#[inline]
pub async fn installed_manifest(
    library: &Library,
    account_id: &str,
    app_name: &str,
) -> anyhow::Result<Option<Manifest>> {
    let Some((_, _, saved)) = installed_files(library, account_id, app_name)? else {
        return Ok(None);
    };
    read_saved(&saved).await.map(Some)
}

async fn read_saved(path: &Path) -> anyhow::Result<Manifest> {
    let raw = tokio::fs::read(path)
        .await
        .with_context(|| format!("reading {}", path.display()))?;
    tokio::task::spawn_blocking(move || parse_manifest(&raw))
        .await
        .context("reading the saved manifest stopped")?
        .context("the saved manifest was refused")
}

/// What the staging's log says of an earlier run.
struct Journal {
    log: LineLog,
    /// Records of the files finished.
    done: HashSet<String>,
    /// Chunks written into the part files kept, when they are of this build.
    applied: HashSet<Guid>,
    /// The log was written for this same manifest, so its part files and chunks still hold.
    is_same_build: bool,
}

/// Opens the staging's log, or starts one, keyed to the manifest by its SHA-1: a log of another
/// build keeps its finished files, which are checked by content, and drops its chunks and parts,
/// whose places in the files belong to that build.
async fn open_log(staging: &Path, manifest_id: &Sha1Digest) -> anyhow::Result<Journal> {
    let path = staging.join(LOG);
    let build_record = format!("manifest {manifest_id}");
    if !path.is_file() {
        let mut log = LineLog::create(&path, LOG_FIRST)
            .await
            .with_context(|| format!("creating {}", path.display()))?;
        log.append(build_record.as_bytes())
            .await
            .with_context(|| format!("writing {}", path.display()))?;
        return Ok(Journal {
            log,
            done: HashSet::new(),
            applied: HashSet::new(),
            is_same_build: true,
        });
    }
    let (mut log, records) = LineLog::open(&path)
        .await
        .with_context(|| format!("opening {}", path.display()))?;
    let mut done = HashSet::new();
    let mut applied = HashSet::new();
    let mut build = None;
    let mut unknown = 0_usize;
    for record in records.into_iter().filter(|record| record.as_slice() != LOG_FIRST) {
        let Ok(text) = String::from_utf8(record) else {
            unknown = unknown.saturating_add(1);
            continue;
        };
        if text.starts_with("done ") {
            done.insert(text);
        } else if let Some(guid) = fetch::applied_chunk(&text) {
            applied.insert(guid);
        } else if text.starts_with("manifest ") {
            build = Some(text);
        } else {
            unknown = unknown.saturating_add(1);
        }
    }
    if unknown > 0 {
        tracing::warn!(path = %path.display(), unknown, "skipped records the epic download log does not know");
    }
    let is_same_build = build.as_deref() == Some(build_record.as_str());
    if !is_same_build {
        tracing::info!(
            dropped_chunks = applied.len(),
            "epic download carried on with another build; its part files start over"
        );
        applied.clear();
        log.append(build_record.as_bytes())
            .await
            .with_context(|| format!("writing {}", path.display()))?;
    }
    Ok(Journal {
        log,
        done,
        applied,
        is_same_build,
    })
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
    fn total_bytes(&self, manifest: &Manifest) -> u64 {
        self.fetch
            .iter()
            .filter_map(|&index| manifest.files().get(index))
            .fold(self.staged_bytes(manifest), |sum, file| sum.saturating_add(file.size()))
    }

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
        let changed = saved.as_deref().map(|old| manifest::changed(&manifest, old));
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

/// Deletes the files the new build dropped, then moves every finished file into the game's folder;
/// deleting first, since a file renamed only in case would otherwise be deleted after it lands on a
/// case-insensitive disk.
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
            "deleted the files the new epic build dropped"
        );
    }
    for file in moved.iter().filter_map(|&index| manifest.files().get(index)) {
        let staged = disk::inside(files_dir, file.path())?;
        let destination = disk::inside(dest, file.path())?;
        disk::move_into_place(&staged, &destination)
            .with_context(|| format!("moving {} into the game's folder", file.path().as_str()))?;
    }
    tracing::debug!(files = moved.len(), "moved the epic download into the library");
    Ok(())
}

fn new_record(target: Target<'_>, install_dir: String, build_version: &str, size: u64) -> InstalledGame {
    InstalledGame {
        account_id: target.account_id.to_owned(),
        storefront: Storefront::EpicGames,
        game_id: target.app_name.to_owned(),
        name: target.title.to_owned(),
        install_dir,
        build_id: build_version.to_owned(),
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
                tracing::warn!(staging = %staging.display(), error = %err, "couldn't delete a cancelled epic download");
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
pub async fn discard(library: &Library, account_id: &str, app_name: &str) -> anyhow::Result<()> {
    for root in library.roots() {
        let staging = disk::staging_dir(&root, account_id, app_name)?;
        if !staging.is_dir() {
            continue;
        }
        let held = decanter_runtime::journal::lock(&staging).context("the epic download is still running")?;
        tokio::fs::remove_dir_all(&staging)
            .await
            .with_context(|| format!("deleting {}", staging.display()))?;
        drop(held);
        tracing::info!(account_id, app_name, staging = %staging.display(), "discarded a stopped epic download");
    }
    Ok(())
}

/// The installed game's folder and its saved manifest.
fn installed_files(
    library: &Library,
    account_id: &str,
    app_name: &str,
) -> anyhow::Result<Option<(InstalledGame, PathBuf, PathBuf)>> {
    let Some(game) = library.read_manifest(account_id, Storefront::EpicGames, app_name) else {
        return Ok(None);
    };
    let dest = library
        .install_dir(&game.install_dir)
        .with_context(|| format!("the folder {:?} of the installed game is unusable", game.install_dir))?;
    let root = library
        .root_of(account_id, app_name)
        .context("the install record's library folder is gone")?;
    let saved = disk::saved_manifest(&root, app_name)?;
    Ok(Some((game, dest, saved)))
}

/// Hashes every file of `manifest` under `dest`.
fn hash_files(manifest: &Manifest, dest: &Path) -> VerifyReport {
    let mut report = VerifyReport::new(manifest.files().len());
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
            report
                .broken
                .push(BrokenFile::new(file.path().as_str().to_owned(), reason));
        }
    }
    report
}

/// Hashes every file of the installed game against the manifest it was installed from, without
/// asking Epic.
///
/// # Errors
///
/// When the game is not installed, or its saved manifest is missing or refused.
#[inline]
pub async fn verify(library: &Library, account_id: &str, app_name: &str) -> anyhow::Result<VerifyReport> {
    let (_, dest, saved) = installed_files(library, account_id, app_name)?.context("the Epic game is not installed")?;
    let manifest = read_saved(&saved).await?;
    let report = tokio::task::spawn_blocking(move || hash_files(&manifest, &dest))
        .await
        .context("verifying the files stopped")?;
    tracing::info!(
        account_id,
        app_name,
        files_checked = report.files_checked,
        broken = report.broken.len(),
        "verified an epic game"
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
pub async fn uninstall(library: &Library, account_id: &str, app_name: &str) -> anyhow::Result<()> {
    discard(library, account_id, app_name).await?;
    let Some((game, dest, saved)) = installed_files(library, account_id, app_name)? else {
        tracing::info!(account_id, app_name, "epic game not installed, nothing to uninstall");
        return Ok(());
    };
    if library.install_dir_is_shared(&game.install_dir, account_id, app_name) {
        tracing::info!(account_id, app_name, dest = %dest.display(), "epic game folder shared with another install, files kept");
    } else {
        match read_saved(&saved).await {
            Ok(manifest) => {
                let folder = dest.clone();
                let removal = tokio::task::spawn_blocking(move || {
                    let paths = manifest.files().iter().map(crate::manifest::ManifestFile::path);
                    let removal = disk::remove_files(&folder, paths.clone());
                    let pruned = disk::prune_dirs(&folder, paths, &[]);
                    let is_folder_gone = std::fs::remove_dir(&folder).is_ok();
                    (removal, pruned, is_folder_gone)
                })
                .await
                .context("deleting the game's files stopped")?;
                let (removal, pruned, is_folder_gone) = removal;
                tracing::info!(
                    account_id,
                    app_name,
                    removed = removal.removed,
                    missing = removal.missing,
                    failed = removal.failed,
                    pruned,
                    is_folder_gone,
                    "deleted an epic game's files"
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
                app_name,
                dest = %dest.display(),
                error = %format_args!("{err:#}"),
                "no usable saved epic manifest, the game's files are left in place"
            ),
        }
    }
    library
        .remove_manifest(account_id, Storefront::EpicGames, app_name)
        .context("removing the install record")?;
    if let Err(err) = tokio::fs::remove_file(&saved).await
        && err.kind() != io::ErrorKind::NotFound
    {
        tracing::warn!(path = %saved.display(), error = %err, "couldn't delete the saved epic manifest");
    }
    tracing::info!(account_id, app_name, "epic game uninstalled");
    Ok(())
}
