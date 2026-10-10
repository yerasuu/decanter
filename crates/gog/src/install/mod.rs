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
//! [`tracking`] is the process-wide bookkeeping [`cancel`] needs,
//! [`program`] guesses which file launches the game once it's all in
//! place, and [`rate`] smooths the transfer rate reported back.

use anyhow::Context;
use decanter_core::{GameArt, InstalledDlc, InstalledGame, Library, Storefront};

mod download;
mod plan;
mod program;
mod rate;
mod staging;
mod tracking;
mod verify;

pub use tracking::cancel;
pub use verify::{BrokenFile, BrokenReason, VerifyReport, repair, verify};

use download::{Cancel, PlannedDownload, ProgressUpdate, download_all};
use plan::Plan;
use rate::RateTracker;
use staging::{move_into_library, staging_root};
use tracking::{InstallKey, active, cancel_requested};

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
    let key: InstallKey = (account_id.to_string(), product_id.to_string());
    tracing::info!(product_id, title, parent_id, branch, build_id, "starting gog install");

    // A second install of the same game would enqueue a second writer for
    // every one of its files. Nothing downstream guards against that, so
    // stop whatever is already running first; a no-op when nothing is.
    cancel(account_id, product_id).await;
    // That call's job is stopping a *previous* run of this key, not
    // recording a cancellation of the one about to start -- clear
    // whatever it just set so this fresh attempt isn't stillborn.
    cancel_requested().lock().await.remove(&key);

    // The depot API needs a live access token; any authenticated call
    // through `with_session` refreshes and re-persists one for us.
    let access_token = crate::achv_try!(
        crate::with_session(account_id, |client| {
            crate::achv_try!(
                client
                    .get_user_data()
                    .map_err(|err| anyhow::anyhow!("GOG session is not valid: {err}"))
            );
            Ok(client.token.borrow().access_token.clone())
        })
        .await
        .inspect_err(|err| tracing::error!(product_id, error = %err, "gog session refresh failed, aborting install"))
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
            &access_token,
            build_owner,
            game_id,
            effective_branch.as_deref(),
            effective_build_id,
        )
        .await
        .inspect_err(
            |err| tracing::error!(product_id, error = %err, "resolving the install plan failed, aborting install")
        )
    );

    // The session refresh and plan fetch just above are exactly the
    // "Queued" window the UI shows a Cancelar button for, before any
    // download exists for `cancel` to reach through `active()`. Honor a
    // cancel that landed during it here, instead of installing anyway.
    if cancel_requested().lock().await.remove(&key) {
        tracing::info!(product_id, "install cancelled before download started");
        anyhow::bail!("install cancelled");
    }

    install_into(
        library,
        account_id,
        product_id,
        parent_id,
        title,
        effective_branch.as_deref(),
        plan,
        &mut on_progress,
    )
    .await
}

#[expect(
    clippy::too_many_arguments,
    reason = "same shape as install's own, which this is the guts of"
)]
async fn install_into(
    library: &Library,
    account_id: &str,
    product_id: &str,
    parent_id: Option<&str>,
    title: &str,
    branch: Option<&str>,
    plan: Plan,
    on_progress: &mut (impl FnMut(Progress) + Send + 'static),
) -> anyhow::Result<()> {
    // A DLC has no directory of its own: its files belong in the base
    // game's, so the base game has to be installed first. Refused here,
    // before anything is downloaded, rather than landing files somewhere
    // nothing would ever launch from.
    let parent = match parent_id {
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

    let key: InstallKey = (account_id.to_string(), product_id.to_string());

    // Anything left from an earlier attempt would otherwise be treated as
    // already-downloaded and moved in as if it were complete.
    let staging = crate::achv_try!(staging_root(account_id, product_id));
    if let Err(err) = tokio::fs::remove_dir_all(&staging).await {
        if err.kind() != std::io::ErrorKind::NotFound {
            tracing::warn!(path = %staging.display(), error = %err, "couldn't clear stale staging directory");
        }
    }

    let in_flight = Cancel::default();
    active().lock().await.insert(key.clone(), in_flight.clone());

    let file_count = plan.files.len();
    let total_size = plan.install_size_bytes;
    let downloads: Vec<PlannedDownload> = plan
        .files
        .iter()
        .map(|file| PlannedDownload {
            relative_path: file.relative_path.clone(),
            chunks: file.chunks.clone(),
        })
        .collect();

    // Every file reports its own running total; the loop below keeps the
    // latest per file and sums them, so what reaches the caller is the
    // install's overall position rather than one arbitrary file's. Only
    // bytes travel through this channel, not each file's own rate -- see
    // `rate::RateTracker` for why that's measured at the install level
    // instead of summed from what each file reports.
    let (progress_tx, mut progress_rx) = tokio::sync::mpsc::channel::<ProgressUpdate>(32);
    let staging_for_worker = staging.clone();
    let worker = tokio::spawn({
        let in_flight = in_flight.clone();
        async move { download_all(downloads, &staging_for_worker, in_flight, progress_tx).await }
    });

    let mut per_file_bytes = vec![0u64; file_count];
    let mut rate_tracker = RateTracker::new();
    while let Some((index, path, bytes)) = progress_rx.recv().await {
        per_file_bytes[index] = bytes;
        let bytes_downloaded = per_file_bytes.iter().sum();
        on_progress(Progress {
            current_file: path,
            bytes_downloaded,
            total_bytes: total_size,
            bytes_per_second: rate_tracker.sample(bytes_downloaded),
        });
    }

    // Cleared before any `?` below: leaving a stale entry would make the
    // next `cancel` for this game try to cancel a finished install.
    let outcome = worker.await;
    active().lock().await.remove(&key);
    let per_file = match outcome {
        Ok(Ok(per_file)) => per_file,
        Ok(Err(err)) => {
            tracing::error!(product_id, error = %err, "install download failed, aborting install");
            if let Err(err) = tokio::fs::remove_dir_all(&staging).await {
                tracing::warn!(path = %staging.display(), error = %err, "couldn't clean up staging after a failed install");
            }
            return Err(err);
        }
        Err(err) => {
            tracing::error!(product_id, error = %err, "install worker panicked, aborting install");
            if let Err(err) = tokio::fs::remove_dir_all(&staging).await {
                tracing::warn!(path = %staging.display(), error = %err, "couldn't clean up staging after a panicked install");
            }
            return Err(anyhow::Error::new(err).context("install worker panicked"));
        }
    };

    let downloaded: u64 = per_file.iter().sum();
    let relative_paths: Vec<String> = plan.files.iter().map(|file| file.relative_path.clone()).collect();

    // Everything downloaded: only now does anything appear in the
    // library, so an install is all-or-nothing.
    crate::achv_try!(
        move_into_library(&staging, &destination_root)
            .await
            .inspect_err(|err| tracing::error!(product_id, error = %err, "moving the download into the library failed"))
            .context("moving the downloaded game into the library")
    );
    if let Err(err) = tokio::fs::remove_dir_all(&staging).await {
        tracing::warn!(path = %staging.display(), error = %err, "couldn't remove staging directory after install");
    }

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
    if let Some(mut parent) = parent {
        parent.dlcs.insert(
            product_id.to_string(),
            InstalledDlc {
                title: title.to_string(),
                build_id: plan.version,
            },
        );
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
        return Ok(());
    }

    let launch_executable =
        program::find_launch_executable(&destination_root, &relative_paths, &plan.install_directory, product_id);

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
                name: title.to_string(),
                install_dir: install_dir_name,
                build_id: plan.version,
                size_on_disk: plan.install_size_bytes.or(Some(downloaded)),
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
            })
            .inspect_err(|err| tracing::error!(product_id, error = %err, "writing the appmanifest failed"))
            .context("writing the appmanifest")
    );

    tracing::info!(product_id, file_count, bytes = downloaded, "install completed");
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
        tracing::info!("keeping {} — another profile still has it installed", game.install_dir);
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

    // Any authenticated call refreshes and re-persists a live token.
    let access_token = crate::achv_try!(
        crate::with_session(account_id, |client| {
            crate::achv_try!(
                client
                    .get_user_data()
                    .map_err(|err| anyhow::anyhow!("GOG session is not valid: {err}"))
            );
            Ok(client.token.borrow().access_token.clone())
        })
        .await
        .inspect_err(|err| tracing::error!(product_id, parent_id, error = %err, "gog session refresh failed, aborting dlc uninstall"))
    );

    let files = crate::achv_try!(
        plan::dlc_only_files(&access_token, parent_numeric, dlc, parent.branch.as_deref())
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
