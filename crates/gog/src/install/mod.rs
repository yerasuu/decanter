//! Installing a GOG game, in-process, with no external plugin registry to
//! round-trip through: [`crate::depot`] for the manifest, [`download`] for
//! chunk transfer, and an appmanifest in decanter's own `steamapps`-shaped
//! library ([`decanter_core::Library`]) as the record of what landed.
//!
//! Files go to `common/<InstallDir>`, named by GOG's own install
//! directory — shared between profiles, outside any prefix. Nothing here
//! creates a prefix: that happens on first launch.
//!
//! Split by concern: [`plan`] resolves what to download, [`download`]
//! fetches it, [`staging`] is where it lands before the library,
//! [`journal`] records how far it got so it can stop and carry on,
//! [`unfinished`] finds the downloads that did stop,
//! [`tracking`] is the process-wide bookkeeping [`cancel`] and [`pause`] need,
//! [`program`] guesses which file launches the game once it's all in
//! place, and [`rate`] smooths the transfer rate reported back.

use std::path::Path;

use anyhow::Context;
use decanter_core::{GameArt, InstalledDlc, InstalledGame, Library, Storefront, platform};

mod download;
mod journal;
mod plan;
mod program;
mod rate;
mod staging;
mod tracking;
mod unfinished;
mod verify;

pub use download::Cancelled;
pub use journal::{PauseReason, Paused, Room};
pub use tracking::{cancel, pause, pause_for_quit};
pub use unfinished::{
    NothingToResume, StrayStaging, UnfinishedDownload, discard, discard_stray, downloading_dir, resume, resume_repair,
    stray_staging, unfinished_downloads,
};
pub use verify::{BrokenFile, BrokenReason, VerifyReport, repair, verify};

use download::{Cancel, PlannedDownload, ProgressUpdate, Unreachable, download_all};
use journal::{Header, Journal, Kind, Opened, Request, Start};
use plan::Plan;
use rate::RateTracker;
use staging::{move_into_library, staging_root};
use tracking::{InstallKey, Settling, Stop, active, claim};

/// Kept free beyond what a download still needs, at least: the disk is never filled to its last
/// byte, which other programs (and the move into the library) need too.
const MIN_MARGIN: u64 = 1024 * 1024 * 1024;

/// Progress for the install as a whole, not for one file: several files
/// transfer at once, so per-file numbers jump around and never add up to
/// something a progress bar can use. `bytes_downloaded` and
/// `bytes_per_second` are summed across every file in flight.
#[derive(Debug, Clone)]
pub struct Progress {
    /// Whichever file most recently reported — a liveness hint, not a
    /// position in the install.
    pub current_file: String,
    pub bytes_downloaded: u64,
    /// The install's full size, known upfront from the depot manifest.
    pub total_bytes: Option<u64>,
    pub bytes_per_second: f64,
}

/// Downloads and installs `product_id` for `account_id`, reporting
/// transfer progress through `on_progress` as it goes.
///
/// There is no prefix to choose: the files are shared, and the prefix is
/// derived from (profile, game) and created on first launch.
///
/// `parent_id` marks this as a DLC of that game, which changes where the
/// files land and what gets recorded: a DLC installs into the base game's
/// directory and is written into the base game's manifest, because on disk
/// there is one directory and one game.
///
/// `title` is the catalog name to record. It comes from the caller
/// because the depot manifest only carries an install directory, which is
/// a folder name — and for a DLC is the *base game's* folder name, so it
/// says nothing about the DLC at all.
///
/// `branch` pins a specific GOG branch (a beta, typically) for the *base
/// game*. Ignored for a DLC install: a DLC ships inside whichever build
/// the base game is already on, so its branch is read from the base
/// game's own manifest rather than taken from the caller — asking to
/// install a DLC on a different branch than its game is on isn't a
/// request that means anything.
///
/// `build_id` pins one exact historical build within that branch instead
/// of always taking the newest one — a manual rollback. Also ignored for
/// a DLC install, for the same reason `branch` is: there is no "this DLC
/// on an older build" to ask for, only the base game's.
///
/// On success an appmanifest records the install — where the files are,
/// which build (and branch), and, when it could be identified, what
/// launches it.
///
/// What a download fetched is journalled in its staging folder, so a later
/// `install` of the same product carries on from it, keeping what a newer
/// build GOG plans still shares, and finishes a move into the library a
/// crash cut short.
///
/// One download runs at a time: this waits for the ones asked for before it.
///
/// # Errors
/// [`Cancelled`] after [`cancel`], which deletes what was downloaded;
/// [`Paused`] after [`pause`], when its disk lacks the room it still needs,
/// or when the network, the disk or the library folder gave out, keeping it.
///
/// `library` is the caller's to resolve (a profile can pick its own
/// location — see `decanter_core::Library`'s own callers for how) rather
/// than assumed here, so this crate has no opinion of its own on where
/// anything lives.
pub async fn install(
    library: &Library,
    account_id: &str,
    product_id: &str,
    parent_id: Option<&str>,
    title: &str,
    branch: Option<&str>,
    build_id: Option<&str>,
    mut on_progress: impl FnMut(Progress) + Send + 'static,
) -> anyhow::Result<()> {
    let game_id: i64 = crate::achv_try!(product_id.parse().context("GOG product_id must be numeric"));
    tracing::info!(product_id, title, parent_id, branch, build_id, "starting gog install");

    claim(account_id, product_id).await;
    let _turn = crate::achv_try!(take_turn(&(account_id.to_owned(), product_id.to_owned())).await);

    // The depot API needs a live access token, renewed and stored first when it has expired.
    let access_token = crate::achv_try!(
        crate::session::live_token(account_id)
            .await
            .map(|token| token.access_token)
            .inspect_err(
                |err| tracing::error!(product_id, error = %err, "gog session refresh failed, aborting install")
            )
    );

    // A DLC's content lives in the base game's build, so that is whose
    // build gets read; which depots come out of it is the DLC's own id.
    // Its branch is inherited from that same manifest, not from the
    // caller — see `install`'s own doc for why.
    let (build_owner, effective_branch) = match parent_id {
        Some(parent_id) => {
            let owner: i64 = crate::achv_try!(parent_id.parse().context("GOG parent product id must be numeric"));
            let parent_branch = library
                .read_manifest(account_id, Storefront::Gog, parent_id)
                .and_then(|parent| parent.branch);
            (owner, parent_branch)
        }
        None => (game_id, branch.map(str::to_string)),
    };
    // A DLC has no build of its own to pin -- see `install`'s own doc.
    let effective_build_id = parent_id.is_none().then_some(build_id).flatten();
    let plan = crate::achv_try!(
        plan::plan(
            access_token.expose(),
            build_owner,
            game_id,
            effective_branch.as_deref(),
            effective_build_id,
            platform::game_system(),
        )
        .await
        .inspect_err(
            |err| tracing::error!(product_id, error = %err, "resolving the install plan failed, aborting install")
        )
    );

    let request = Request {
        kind: Kind::Install,
        account_id: account_id.to_owned(),
        product_id: product_id.to_owned(),
        parent_id: parent_id.map(str::to_owned),
        title: title.to_owned(),
        branch: branch.map(str::to_owned),
        build_id: build_id.map(str::to_owned),
    };
    install_into(library, request, effective_branch.as_deref(), plan, &mut on_progress).await
}

/// Downloads `request`'s plan, or carries on its journal, and lands it in
/// the library. `branch` is the one resolved for it, which is what the
/// manifest records.
async fn install_into(
    library: &Library,
    request: Request,
    branch: Option<&str>,
    plan: Plan,
    on_progress: &mut (impl FnMut(Progress) + Send + 'static),
) -> anyhow::Result<()> {
    let account_id = request.account_id.clone();
    let product_id = request.product_id.clone();
    let (account_id, product_id) = (account_id.as_str(), product_id.as_str());
    let title = request.title.clone();
    // A DLC has no directory of its own: its files belong in the base
    // game's, so the base game has to be installed first. Refused here,
    // before anything is downloaded, rather than landing files somewhere
    // nothing would ever launch from.
    let parent = match request.parent_id.as_deref() {
        Some(parent_id) => Some(crate::achv_try!(
            library
                .read_manifest(account_id, Storefront::Gog, parent_id)
                .with_context(|| format!("install the base game before its DLC (product {parent_id})"))
                .inspect_err(|err| tracing::error!(product_id, parent_id, error = %err, "dlc install aborted"))
        )),
        None => None,
    };

    // GOG's own directory name, so the layout under `common/` reads the
    // way the game's own installer would have written it.
    let install_dir_name = match &parent {
        Some(parent) => parent.install_dir.clone(),
        None => plan.install_directory.clone(),
    };
    let destination_root = crate::achv_try!(
        library
            .install_dir(&install_dir_name)
            .with_context(|| format!("unusable install directory {install_dir_name:?}"))
            .inspect_err(|err| tracing::error!(product_id, install_dir_name, error = %err, "install aborted"))
    );

    let key: InstallKey = (account_id.to_owned(), product_id.to_owned());
    let staging = crate::achv_try!(staging_root(&destination_root, account_id, product_id));
    let downloads = plan.downloads();
    let header = crate::achv_try!(Header::new(
        request,
        plan.version.clone(),
        plan.install_directory.clone(),
        &downloads
    ));
    let (journal, header) =
        crate::achv_try!(fetch_into_staging(&staging, header, downloads, key, product_id, on_progress).await);

    // Everything downloaded: only now does anything appear in the
    // library, so an install is all-or-nothing.
    crate::achv_try!(move_staged(&journal, &destination_root, product_id).await);

    let relative_paths: Vec<String> = header.files.iter().map(|file| file.path.clone()).collect();
    let file_count = relative_paths.len();
    let downloaded = header.total_bytes;

    // Best-effort and after the fact rather than folded into the
    // download/move above: most installs have nothing to hook at all,
    // and a failure here (a locked file, an odd permission) shouldn't
    // undo bytes that already landed correctly.
    match crate::hook::deploy(&destination_root, &relative_paths).await {
        Ok(true) => tracing::info!(product_id, "achievement hook deployed"),
        Ok(false) => {}
        Err(err) => tracing::warn!(product_id, error = %err, "failed to deploy the achievement hook"),
    }

    // A DLC is recorded on the game it went into. Its own launch
    // executable is not looked for: whatever a DLC ships, the thing you
    // start is still the base game.
    if let Some(parent) = parent {
        let dlc = InstalledDlc {
            title,
            build_id: header.build_id.clone(),
        };
        crate::achv_try!(record_dlc(library, parent, product_id, dlc, downloaded, file_count));
        finish(journal, product_id).await;
        return Ok(());
    }

    let launch_executable = program::find_launch_executable(
        &destination_root,
        &relative_paths,
        &header.install_directory,
        product_id,
        plan.system,
    );

    // A reinstall (an update, a branch switch) must not drop what only
    // lives on the previous record: the DLC already sitting in the
    // directory, which Proton build was pinned, when it was last played.
    let previous = library.read_manifest(account_id, Storefront::Gog, product_id);
    let dlcs = previous.as_ref().map(|game| game.dlcs.clone()).unwrap_or_default();
    let proton = previous.as_ref().and_then(|game| game.proton.clone());
    let last_played = previous.as_ref().and_then(|game| game.last_played);
    let cloud_save_path = previous.as_ref().and_then(|game| game.cloud_save_path.clone());
    let cloud_save_last_sync = previous.as_ref().and_then(|game| game.cloud_save_last_sync);

    crate::achv_try!(
        library
            .write_manifest(&InstalledGame {
                account_id: account_id.to_string(),
                storefront: Storefront::Gog,
                game_id: product_id.to_string(),
                name: title,
                install_dir: install_dir_name,
                build_id: header.build_id.clone(),
                size_on_disk: Some(downloaded),
                size_fingerprint: None,
                launch_executable,
                last_played,
                proton,
                dlcs,
                branch: branch.map(str::to_string),
                cloud_save_path,
                cloud_save_last_sync,
                art: GameArt::default(),
                umu_id: None,
                umu_store: None,
                prefix_of: None,
            })
            .inspect_err(|err| tracing::error!(product_id, error = %err, "writing the appmanifest failed"))
            .context("writing the appmanifest")
    );

    finish(journal, product_id).await;
    tracing::info!(product_id, file_count, bytes = downloaded, "install completed");
    Ok(())
}

/// Waits for this download's turn. A stop asked while it waited lets it go on
/// without one: it is still planned and its journal opened, where the stop
/// lands before any byte moves, so a paused download always has a journal to
/// resume from. Quitting stops it at once instead.
async fn take_turn(key: &InstallKey) -> anyhow::Result<Option<tokio::sync::SemaphorePermit<'static>>> {
    match tracking::wait_turn(key).await {
        Some(turn) => Ok(Some(turn)),
        None if tracking::is_quitting() => Err(Paused {
            reason: PauseReason::AppClosed,
            bytes_done: 0,
            root: None,
            room: None,
        }
        .into()),
        None => Ok(None),
    }
}

/// The room a download still needs on `staging`'s disk against what it has:
/// what is left of it, plus the larger of [`MIN_MARGIN`] and 2 % of its size
/// (#1277, §2.5). An update counts whole, since staging holds a second copy
/// of the game until the move. `None` when the free space can't be read.
fn room(staging: &Path, total: u64, done: u64) -> Option<Room> {
    let Some((free, _)) = decanter_core::disk_usage(staging) else {
        return None;
    };
    let margin = total.div_ceil(50).max(MIN_MARGIN);
    Some(Room::new(total.saturating_sub(done).saturating_add(margin), free))
}

/// Opens `staging`'s journal for `header` and fetches whatever it still
/// lacks under `key`'s stop flag, reporting the whole set's progress;
/// shared by installs and repairs so both stop and carry on the same way.
///
/// Returns the journal and the header of what is staged: the earlier plan
/// when a crash hit its move into the library, which is then finished
/// rather than downloaded again.
///
/// # Errors
/// [`Cancelled`] after a confirmed cancel, which deletes the download;
/// [`Paused`] when it stopped and kept its data (a pause, the network, a
/// full disk or too little room to start, a library gone); any other
/// failure keeps the data too.
async fn fetch_into_staging(
    staging: &Path,
    header: Header,
    downloads: Vec<PlannedDownload>,
    key: InstallKey,
    product_id: &str,
    on_progress: &mut (impl FnMut(Progress) + Send + 'static),
) -> anyhow::Result<(Journal, Header)> {
    let planned_bytes = header.total_bytes;
    let (journal, header, starts) = match journal::open(staging, header).await {
        Ok(Opened::Move { journal, header }) => return Ok((journal, header)),
        Ok(Opened::Download {
            journal,
            header,
            starts,
        }) => (journal, header, starts),
        Err(err) => {
            tracing::error!(product_id, error = %err, "opening the download journal failed");
            return Err(pause_reason(&err, staging).await.map_or(err, |reason| {
                Paused {
                    reason,
                    bytes_done: 0,
                    root: library_root(staging),
                    room: (reason == PauseReason::DiskFull)
                        .then(|| room(staging, planned_bytes, 0))
                        .flatten(),
                }
                .into()
            }));
        }
    };

    // In `active()` before the queued check, so a stop landing between the
    // two still reaches the download.
    let in_flight = Cancel::default();
    let _settling = Settling::start();
    active().lock().await.insert(key.clone(), in_flight.clone());

    // The session refresh and plan fetch before this are exactly the
    // "Queued" window the UI shows Cancelar for, before any download
    // exists for `cancel` or `pause` to reach through `active()`.
    match tracking::stop_while_queued(&key).await {
        Some(Stop::Cancel) => in_flight.cancel(),
        Some(Stop::Pause) => in_flight.pause(),
        Some(Stop::Quit) => in_flight.quit(),
        None if tracking::is_quitting() => in_flight.quit(),
        None => {}
    }

    // Every file reports its own running total; the loop below keeps the
    // latest per file and sums them, so what reaches the caller is the
    // install's overall position rather than one arbitrary file's. Only
    // bytes travel through this channel, not each file's own rate -- see
    // `rate::RateTracker` for why that's measured at the install level
    // instead of summed from what each file reports.
    let mut per_file_bytes: Vec<u64> = starts
        .iter()
        .zip(&downloads)
        .map(|(start, download)| match start {
            Start::Done => download.size(),
            Start::At { bytes, .. } => *bytes,
        })
        .collect();
    let bytes_before: u64 = per_file_bytes.iter().sum();
    let lacking = room(staging, header.total_bytes, bytes_before).filter(|room| room.short() > 0);
    let outcome = if in_flight.requested().is_some() {
        tracing::info!(product_id, "download stopped before it started");
        Err(Cancelled.into())
    } else if let Some(lacking) = lacking {
        tracing::warn!(
            product_id,
            needed = lacking.needed,
            free = lacking.free,
            "not enough free space to download, pausing before it starts"
        );
        Err(std::io::Error::new(
            std::io::ErrorKind::StorageFull,
            "not enough free space for the download",
        )
        .into())
    } else {
        let (progress_tx, mut progress_rx) = tokio::sync::mpsc::channel::<ProgressUpdate>(32);
        let worker = download_all(downloads, starts, &journal, in_flight.clone(), progress_tx);
        let mut rate_tracker = RateTracker::new(bytes_before);
        let total_bytes = Some(header.total_bytes);
        let per_file_bytes = &mut per_file_bytes;
        let report = async move {
            while let Some((index, path, bytes)) = progress_rx.recv().await {
                if let Some(slot) = per_file_bytes.get_mut(index) {
                    *slot = bytes;
                }
                let bytes_downloaded = per_file_bytes.iter().sum();
                on_progress(Progress {
                    current_file: path,
                    bytes_downloaded,
                    total_bytes,
                    bytes_per_second: rate_tracker.sample(bytes_downloaded),
                });
            }
        };
        futures_util::future::join(worker, report).await.0.map(drop)
    };

    // Only this run's own flag: a newer run of the same key may have
    // claimed it meanwhile, and its entry is what a stop must reach.
    {
        let mut active = active().lock().await;
        if active.get(&key).is_some_and(|flag| flag.is_same(&in_flight)) {
            active.remove(&key);
        }
    }
    match outcome {
        Ok(()) => Ok((journal, header)),
        Err(err) => {
            let bytes_done = per_file_bytes.iter().sum();
            Err(settle(journal, &header, err, &in_flight, bytes_done, staging).await)
        }
    }
}

/// What a download that stopped short ends as: a confirmed cancel deletes
/// it, a pause or a cause a resume can outlast keeps it with that reason
/// recorded, and any other failure fails, keeping it too (#1281).
async fn settle(
    journal: Journal,
    header: &Header,
    err: anyhow::Error,
    in_flight: &Cancel,
    bytes_done: u64,
    staging: &Path,
) -> anyhow::Error {
    let product_id = header.request.product_id.as_str();
    let reason = match in_flight.requested() {
        Some(Stop::Cancel) => {
            tracing::info!(product_id, "download cancelled, removing what it downloaded");
            if let Err(err) = journal.remove().await {
                tracing::warn!(path = %staging.display(), error = %err, "couldn't remove a cancelled download");
            }
            return Cancelled.into();
        }
        Some(Stop::Pause) => Some(PauseReason::User),
        Some(Stop::Quit) => Some(PauseReason::AppClosed),
        None => pause_reason(&err, staging).await,
    };
    let Some(reason) = reason else {
        tracing::error!(product_id, error = %err, "install download failed, keeping what it downloaded");
        return err;
    };
    let room = (reason == PauseReason::DiskFull)
        .then(|| room(staging, header.total_bytes, bytes_done))
        .flatten();
    if let Err(record_err) = journal.paused(reason, room).await {
        tracing::warn!(product_id, error = %record_err, "couldn't record the pause; a resume treats it as a crash");
    }
    tracing::info!(product_id, ?reason, bytes_done, ?room, error = %err, "download paused");
    Paused {
        reason,
        bytes_done,
        root: library_root(staging),
        room,
    }
    .into()
}

/// The library folder a staging folder (`<root>/downloading/<account>/<product>`) belongs to.
fn library_root(staging: &Path) -> Option<std::path::PathBuf> {
    staging.ancestors().nth(3).map(Path::to_path_buf)
}

/// The cause a resume can outlast, if `err` is one.
async fn pause_reason(err: &anyhow::Error, staging: &Path) -> Option<PauseReason> {
    if err.downcast_ref::<Unreachable>().is_some() {
        return Some(PauseReason::Network);
    }
    let disk_full = err
        .chain()
        .filter_map(|cause| cause.downcast_ref::<std::io::Error>())
        .any(|io| {
            matches!(
                io.kind(),
                std::io::ErrorKind::StorageFull | std::io::ErrorKind::QuotaExceeded
            )
        });
    if disk_full {
        return Some(PauseReason::DiskFull);
    }
    // An unplugged disk fails with whatever error its filesystem gives;
    // that the library folder is gone is what tells it apart.
    let library_root = library_root(staging)?;
    let is_there = tokio::fs::metadata(library_root).await.is_ok_and(|meta| meta.is_dir());
    (!is_there).then_some(PauseReason::LibraryUnavailable)
}

/// Marks the journal as moving, so a crash from here on finishes the move
/// instead of downloading again, then moves what is staged into the library.
async fn move_staged(journal: &Journal, destination_root: &Path, product_id: &str) -> anyhow::Result<()> {
    crate::achv_try!(journal.moving().await);
    move_into_library(&journal.files(), destination_root)
        .await
        .inspect_err(|err| tracing::error!(product_id, error = %err, "moving the download into the library failed"))
        .context("moving the downloaded files into the library")
}

/// The library records the download: its journal is done with.
async fn finish(journal: Journal, product_id: &str) {
    if let Err(err) = journal.recorded().await {
        tracing::warn!(product_id, error = %err, "couldn't record the finished install in its journal");
    }
    if let Err(err) = journal.remove().await {
        tracing::warn!(product_id, error = %err, "couldn't remove the staging folder after install");
    }
}

fn record_dlc(
    library: &Library,
    mut parent: InstalledGame,
    product_id: &str,
    dlc: InstalledDlc,
    downloaded: u64,
    file_count: usize,
) -> anyhow::Result<()> {
    parent.dlcs.insert(product_id.to_string(), dlc);
    // The DLC's bytes are in the same directory, so the game's
    // recorded size has to grow by them.
    parent.size_on_disk = Some(parent.size_on_disk.unwrap_or(0) + downloaded);
    crate::achv_try!(
        library
            .write_manifest(&parent)
            .inspect_err(|err| tracing::error!(product_id, error = %err, "recording the installed dlc failed"))
            .context("recording the DLC on its game")
    );
    tracing::info!(product_id, file_count, bytes = downloaded, "install completed (dlc)");
    Ok(())
}

/// Drops the manifest, this profile's prefix for the game, and — only if
/// no other profile still owns it — the files themselves.
///
/// The files are shared, so deleting them is conditional. The prefix is
/// not: it belongs to this (profile, game) alone.
pub async fn uninstall(library: &Library, account_id: &str, product_id: &str) -> anyhow::Result<()> {
    // An in-flight install would keep writing files into the directory
    // this is about to delete, leaving an untracked half-install behind.
    cancel(account_id, product_id).await;

    let Some(game) = crate::achv_try!(
        library
            .remove_manifest(account_id, Storefront::Gog, product_id)
            .context("removing the appmanifest")
    ) else {
        return Ok(());
    };

    if let Some(compatdata) = library.compatdata(account_id, product_id) {
        crate::achv_try!(
            remove_dir(&compatdata)
                .await
                .inspect_err(|err| tracing::error!(product_id, error = %err, "removing the compatdata prefix failed"))
        );
    }

    // Checked after the manifest is gone, so this profile's own record
    // doesn't count as another owner.
    if library.install_dir_is_shared(&game.install_dir, account_id, product_id) {
        tracing::info!(account_id, product_id, install_dir = %game.install_dir, "files kept, another profile still has them installed");
        return Ok(());
    }
    if let Some(directory) = library.install_dir(&game.install_dir) {
        crate::achv_try!(
            remove_dir(&directory)
                .await
                .inspect_err(|err| tracing::error!(product_id, error = %err, "removing the install directory failed"))
        );
    }
    tracing::info!(product_id, "uninstall completed");
    Ok(())
}

/// Removes one DLC from the game it was installed into.
///
/// Only the files the DLC alone shipped are deleted — see
/// [`plan::dlc_only_files`] for why anything the base game also carries
/// has to stay. What's left is the game exactly as it was before the DLC.
///
/// Removing a DLC that isn't recorded is a no-op: that is the state being
/// asked for.
pub async fn uninstall_dlc(
    library: &Library,
    account_id: &str,
    product_id: &str,
    parent_id: &str,
) -> anyhow::Result<()> {
    cancel(account_id, product_id).await;

    let Some(mut parent) = library.read_manifest(account_id, Storefront::Gog, parent_id) else {
        return Ok(());
    };
    if !parent.dlcs.contains_key(product_id) {
        return Ok(());
    }

    let dlc: i64 = crate::achv_try!(product_id.parse().context("GOG product_id must be numeric"));
    let parent_numeric: i64 = crate::achv_try!(parent_id.parse().context("GOG parent product id must be numeric"));

    let access_token = crate::achv_try!(
        crate::session::live_token(account_id)
            .await
            .map(|token| token.access_token)
        .inspect_err(|err| tracing::error!(product_id, parent_id, error = %err, "gog session refresh failed, aborting dlc uninstall"))
    );

    let files = crate::achv_try!(
        plan::dlc_only_files(
            access_token.expose(),
            parent_numeric,
            dlc,
            parent.branch.as_deref(),
            platform::game_system(),
        )
            .await
            .inspect_err(|err| tracing::error!(product_id, parent_id, error = %err, "resolving dlc files failed, aborting dlc uninstall"))
    );
    let root = crate::achv_try!(
        library
            .install_dir(&parent.install_dir)
            .context("unusable install directory")
    );
    let mut removed = 0u64;
    for file in files {
        let path = root.join(file);
        // Sizes are read before deleting so the recorded total can shrink
        // by what actually went.
        match tokio::fs::metadata(&path).await {
            Ok(meta) => removed += meta.len(),
            Err(err) => {
                tracing::warn!(path = %path.display(), error = %err, "couldn't read dlc file size before removing it")
            }
        }
        if let Err(err) = tokio::fs::remove_file(&path).await {
            tracing::warn!(path = %path.display(), error = %err, "couldn't remove dlc file");
        } else {
            tracing::debug!(path = %path.display(), "removed dlc file");
        }
    }

    parent.dlcs.remove(product_id);
    parent.size_on_disk = Some(parent.size_on_disk.unwrap_or(0).saturating_sub(removed));
    crate::achv_try!(
        library
            .write_manifest(&parent)
            .inspect_err(|err| tracing::error!(product_id, parent_id, error = %err, "updating the game after removing its dlc failed"))
            .context("updating the game after removing its DLC")
    );
    tracing::info!(
        product_id,
        parent_id,
        bytes_removed = removed,
        "dlc uninstall completed"
    );
    Ok(())
}

/// Already gone is the outcome being asked for.
async fn remove_dir(path: &std::path::Path) -> anyhow::Result<()> {
    tokio::fs::remove_dir_all(path)
        .await
        .or_else(|err| match err.kind() {
            std::io::ErrorKind::NotFound => Ok(()),
            _ => Err(err),
        })
        .with_context(|| format!("removing {}", path.display()))
}
