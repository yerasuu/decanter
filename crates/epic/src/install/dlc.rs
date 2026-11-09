// SPDX-License-Identifier: GPL-3.0-or-later
//! An Epic DLC: an owned catalog item filed under its game by `mainGameItem` (spec, 4.6 rule 4),
//! installable when the asset list names its own `appName` (rule 5), whose build is asked for by
//! its own namespace, catalog item and `appName` as a game's is (spec, 5.1).
//!
//! The spec says neither where a DLC's files go nor how its manifest relates to its game's, so it
//! is read as GOG's and Steam's DLC are: the files land in the game's folder, the DLC is recorded on
//! the game's install record, and its manifest is saved beside the game's under its own `appName`,
//! which is what uninstalling only the DLC deletes by. A manifest answer that does not look like a
//! DLC's own build is refused with a [`DlcError`] and its shape logged.

use std::collections::{HashMap, HashSet};
use std::io;
use std::path::Path;

use anyhow::Context as _;
use decanter_core::{InstalledDlc, InstalledGame, Library, Storefront};
use decanter_runtime::install::ManifestPath;

use super::{Target, disk, installed_files, keep_manifest, read_saved};
use crate::manifest::{Manifest, ManifestFile};

/// Why a DLC was refused before anything of it was written into its game's folder.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum DlcError {
    /// Its game has no install record on this account, and a DLC's files go into the game's folder.
    #[error("the DLC's game is not installed: install the game first")]
    GameNotInstalled,
    /// Every file of Epic's manifest for the DLC is one its game already has, as it has it: the
    /// answer is not a build of the DLC's own.
    #[error("Epic answered this DLC with a build whose {files} files are all its game's")]
    GameBuild { files: usize },
    #[error("Epic's manifest for this DLC lists no files")]
    NoFiles,
}

/// Refuses a DLC manifest that is not a build of the DLC's own, after logging the counts that tell
/// the two shapes apart, since no real DLC answer has been read yet (spec, 5.1 and Q13).
pub(super) async fn check_shape(
    manifest: &Manifest,
    target: Target<'_>,
    root: &Path,
    game: &InstalledGame,
) -> Result<(), DlcError> {
    let game_manifest = match disk::saved_manifest(root, &game.game_id) {
        Ok(path) => read_saved(&path).await.ok(),
        Err(_) => None,
    };
    let game_files: HashMap<String, &ManifestFile> = game_manifest
        .iter()
        .flat_map(Manifest::files)
        .map(|file| (file.path().folded(), file))
        .collect();
    let (shared, same) = manifest
        .files()
        .iter()
        .filter_map(|file| game_files.get(&file.path().folded()).map(|known| (file, known)))
        .fold((0_usize, 0_usize), |(shared, same), (file, known)| {
            let is_same = known.sha1() == file.sha1() && known.size() == file.size();
            (shared.saturating_add(1), same.saturating_add(usize::from(is_same)))
        });
    let files = manifest.files().len();
    tracing::info!(
        app_name = target.app_name,
        parent_app_name = target.parent_app_name,
        manifest_app = manifest.app_name(),
        is_named_as_dlc = manifest.app_name().eq_ignore_ascii_case(target.app_name),
        is_named_as_game = manifest.app_name().eq_ignore_ascii_case(&game.game_id),
        build_version = manifest.build_version(),
        files,
        bytes = manifest.total_size(),
        is_game_manifest_read = game_manifest.is_some(),
        game_files = game_files.len(),
        shared_with_game = shared,
        same_as_game = same,
        "epic dlc manifest shape"
    );
    if files == 0 {
        tracing::warn!(
            app_name = target.app_name,
            "epic dlc manifest refused: it lists no files"
        );
        return Err(DlcError::NoFiles);
    }
    if game_manifest.is_some() && same == files {
        tracing::warn!(
            app_name = target.app_name,
            files,
            "epic dlc manifest refused: every file is its game's"
        );
        return Err(DlcError::GameBuild { files });
    }
    Ok(())
}

/// Drops from `removed` the paths another manifest of the same folder still lists: an update of
/// the game or of one DLC must not delete a file the others have. When one of their manifests
/// can't be read nothing is deleted, since what it holds is then unknown.
pub(super) async fn spare_shared(root: &Path, game: &InstalledGame, app_name: &str, removed: &mut Vec<ManifestPath>) {
    match other_paths(root, game, app_name).await {
        Some(kept) => {
            let before = removed.len();
            removed.retain(|path| !kept.contains(&path.folded()));
            tracing::debug!(
                app_name,
                spared = before.saturating_sub(removed.len()),
                "kept the dropped files another manifest of the folder lists"
            );
        }
        None => {
            tracing::warn!(
                app_name,
                dropped = removed.len(),
                "a saved manifest of the folder is unusable, the files the new build dropped are left"
            );
            removed.clear();
        }
    }
}

/// Folded paths of every manifest saved for `game`'s folder but `skip`'s: the game's own and its
/// DLC's. `None` when one of them can't be read.
async fn other_paths(root: &Path, game: &InstalledGame, skip: &str) -> Option<HashSet<String>> {
    let mut paths = HashSet::new();
    let names = core::iter::once(game.game_id.as_str())
        .chain(game.dlcs.keys().map(String::as_str))
        .filter(|name| *name != skip);
    for name in names {
        let read = match disk::saved_manifest(root, name) {
            Ok(path) => read_saved(&path).await,
            Err(err) => Err(err.into()),
        };
        match read {
            Ok(manifest) => paths.extend(manifest.files().iter().map(|file| file.path().folded())),
            Err(err) => {
                tracing::warn!(app_name = name, error = %format_args!("{err:#}"), "an epic saved manifest of the folder is unusable");
                return None;
            }
        }
    }
    Some(paths)
}

/// The saved manifests of the DLC recorded on a game.
pub(super) struct DlcManifests {
    /// By `appName`.
    pub read: Vec<(String, Manifest)>,
    /// How many could not be read, each logged.
    pub unusable: usize,
}

pub(super) async fn manifests(root: &Path, game: &InstalledGame) -> DlcManifests {
    let mut found = DlcManifests {
        read: Vec::with_capacity(game.dlcs.len()),
        unusable: 0,
    };
    for name in game.dlcs.keys() {
        let read = match disk::saved_manifest(root, name) {
            Ok(path) => read_saved(&path).await,
            Err(err) => Err(err.into()),
        };
        match read {
            Ok(manifest) => found.read.push((name.clone(), manifest)),
            Err(err) => {
                tracing::warn!(app_name = name, error = %format_args!("{err:#}"), "an epic dlc's saved manifest is unusable");
                found.unusable = found.unusable.saturating_add(1);
            }
        }
    }
    found
}

/// The bytes the DLC recorded on `game` take, by their saved manifests, for the game's recorded
/// size, which counts its DLC.
pub(super) async fn recorded_size(root: &Path, game: &InstalledGame) -> u64 {
    manifests(root, game)
        .await
        .read
        .iter()
        .fold(0, |sum, (_, manifest)| sum.saturating_add(manifest.total_size()))
}

/// Saves the DLC's manifest, then records the DLC on its game, whose size grows by what the DLC
/// grew. The game's record is read again here, since the download may have taken hours.
pub(super) async fn record(
    library: &Library,
    root: &Path,
    target: Target<'_>,
    manifest: &Manifest,
    previous_size: Option<u64>,
    raw: &[u8],
) -> anyhow::Result<()> {
    let parent = target.parent_app_name.ok_or(DlcError::GameNotInstalled)?;
    let mut game = library
        .read_manifest(target.account_id, Storefront::EpicGames, parent)
        .ok_or(DlcError::GameNotInstalled)?;
    keep_manifest(root, target.app_name, raw).await?;
    game.dlcs.insert(
        target.app_name.to_owned(),
        InstalledDlc {
            title: target.title.to_owned(),
            build_id: manifest.build_version().to_owned(),
        },
    );
    game.size_on_disk = game.size_on_disk.map(|size| {
        size.saturating_sub(previous_size.unwrap_or_default())
            .saturating_add(manifest.total_size())
    });
    library
        .clone()
        .targeting(root)
        .write_manifest(&game)
        .context("recording the DLC on its game")?;
    tracing::info!(
        account_id = target.account_id,
        app_name = target.app_name,
        parent_app_name = parent,
        build_version = manifest.build_version(),
        bytes = manifest.total_size(),
        "recorded an epic dlc on its game"
    );
    Ok(())
}

/// Deletes a saved manifest nothing records any more.
pub(super) async fn forget_manifest(root: &Path, app_name: &str) {
    let Ok(path) = disk::saved_manifest(root, app_name) else {
        return;
    };
    if let Err(err) = tokio::fs::remove_file(&path).await
        && err.kind() != io::ErrorKind::NotFound
    {
        tracing::warn!(path = %path.display(), error = %err, "couldn't delete the saved epic manifest");
    }
}

/// Uninstalls one DLC from the game it went into: the files its manifest lists that neither the
/// game's manifest nor another installed DLC's lists, then the folders they leave empty. The game
/// stays as it was before the DLC. A DLC not recorded on the game is already the state asked for.
/// A folder another install shares keeps its files, and so does a DLC whose manifest, or another
/// manifest of the folder, can't be read, since its own files can't then be told apart.
///
/// # Errors
///
/// When files can't be deleted, which keeps the DLC recorded for another try, or the game's record
/// can't be written.
#[inline]
pub async fn uninstall_dlc(
    library: &Library,
    account_id: &str,
    app_name: &str,
    parent_app_name: &str,
) -> anyhow::Result<()> {
    super::discard(library, account_id, app_name).await?;
    let Some((mut game, dest, root)) = installed_files(library, account_id, parent_app_name)? else {
        tracing::info!(
            account_id,
            app_name,
            parent_app_name,
            "epic dlc's game not installed, nothing to uninstall"
        );
        return Ok(());
    };
    if !game.dlcs.contains_key(app_name) {
        tracing::info!(
            account_id,
            app_name,
            parent_app_name,
            "epic dlc not installed, nothing to uninstall"
        );
        return Ok(());
    }
    // The game's recorded size counts the DLC's whole build, as `record` added it.
    let mut dlc_size = 0_u64;
    if library.install_dir_is_shared(&game.install_dir, account_id, parent_app_name) {
        tracing::info!(account_id, app_name, dest = %dest.display(), "epic game folder shared with another install, dlc files kept");
    } else {
        let saved = disk::saved_manifest(&root, app_name)?;
        match (read_saved(&saved).await, other_paths(&root, &game, app_name).await) {
            (Ok(manifest), Some(kept)) => {
                let own: Vec<ManifestPath> = manifest
                    .files()
                    .iter()
                    .map(ManifestFile::path)
                    .filter(|path| !kept.contains(&path.folded()))
                    .cloned()
                    .collect();
                let shared = manifest.files().len().saturating_sub(own.len());
                dlc_size = manifest.total_size();
                let folder = dest.clone();
                let (removal, pruned) = tokio::task::spawn_blocking(move || {
                    let removal = disk::remove_files(&folder, &own);
                    let pruned = disk::prune_dirs(&folder, &own, &[]);
                    (removal, pruned)
                })
                .await
                .context("deleting the DLC's files stopped")?;
                tracing::info!(
                    account_id,
                    app_name,
                    parent_app_name,
                    removed = removal.removed,
                    missing = removal.missing,
                    failed = removal.failed,
                    shared_kept = shared,
                    pruned,
                    "deleted an epic dlc's files"
                );
                anyhow::ensure!(
                    removal.failed == 0,
                    "{} of the DLC's files could not be deleted from {}",
                    removal.failed,
                    dest.display()
                );
            }
            (Err(err), _) => tracing::warn!(
                account_id,
                app_name,
                error = %format_args!("{err:#}"),
                "no usable saved epic dlc manifest, its files are left in place"
            ),
            (Ok(_), None) => tracing::warn!(
                account_id,
                app_name,
                "another saved manifest of the folder is unusable, the dlc's files are left in place"
            ),
        }
    }
    game.dlcs.remove(app_name);
    game.size_on_disk = game.size_on_disk.map(|size| size.saturating_sub(dlc_size));
    library
        .clone()
        .targeting(&root)
        .write_manifest(&game)
        .context("updating the game after removing its DLC")?;
    forget_manifest(&root, app_name).await;
    tracing::info!(account_id, app_name, parent_app_name, dlc_size, "epic dlc uninstalled");
    Ok(())
}
