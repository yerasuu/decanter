// SPDX-License-Identifier: GPL-3.0-or-later
//! Registering an Epic game folder the user already has, as Amazon's import does: the folder is
//! hashed against the live manifest (spec, 5.1 to 6) and, once it matches, recorded as the install
//! without moving or copying a file. The folder is untrusted, so it is held to the shape a record
//! needs and every file is reached through `disk::inside`, which refuses links.

use std::path::Path;

use anyhow::Context as _;
use decanter_core::{InstalledGame, Library, Storefront};
pub use decanter_runtime::install::import::{Differences, Imported};
use decanter_runtime::install::import::{ImportFolder, resolve_folder};

use super::distribution::{self, Asked, ManifestFetch};
use super::{Target, hash_files, is_app_name, launch_program, new_record, read_downloaded, save_record, staged_root};
use crate::library::is_id;

/// Verifies `folder` against the live manifest of `target` and, when it matches (or `differences`
/// accepts that it does not), records it as the game's install where it stands.
///
/// `folder` must be a direct child of a library folder's `common/`, since an install record names
/// its folder by that single path component; one reached through a link is refused. A DLC is not
/// imported on its own: its files are its game's folder's.
///
/// # Errors
///
/// When `target` is a DLC, the game is installed or has a stopped download, the folder is not
/// usable, Epic cannot be asked or answers a manifest that fails its checks, no file of the folder
/// matches, or the record can't be written.
#[inline]
pub async fn import(
    library: &Library,
    target: Target<'_>,
    folder: &Path,
    differences: Differences,
) -> anyhow::Result<Imported> {
    let Target {
        account_id, app_name, ..
    } = target;
    anyhow::ensure!(
        target.parent_app_name.is_none(),
        "a DLC is not imported on its own: import its game"
    );
    anyhow::ensure!(
        is_app_name(app_name) && is_id(target.namespace) && is_id(target.catalog_item_id),
        "the Epic game's ids are not usable"
    );
    anyhow::ensure!(
        library
            .read_manifest(account_id, Storefront::EpicGames, app_name)
            .is_none(),
        "this Epic game is already installed"
    );
    anyhow::ensure!(
        staged_root(library, account_id, app_name).is_none(),
        "this Epic game has a stopped download: resume or cancel it first"
    );
    let ImportFolder {
        root,
        install_dir,
        dest,
        ..
    } = resolve_folder(library, folder)?;
    anyhow::ensure!(
        !library.install_dir_is_shared(&install_dir, account_id, app_name),
        "another game already uses the folder {install_dir:?}"
    );

    let download = match distribution::game_download(&target).await? {
        Asked::Ready(download) => download,
        Asked::SessionClosed => anyhow::bail!("Epic closed the session: log in to Epic Games again in Settings"),
        Asked::Unreachable(reason) => anyhow::bail!("Epic could not be reached: {reason}"),
    };
    let raw = match distribution::manifest(&download).await {
        Ok(raw) => raw,
        Err(ManifestFetch::Unreachable(reason)) => anyhow::bail!("Epic's manifest could not be fetched: {reason}"),
        Err(ManifestFetch::Mismatch) => {
            anyhow::bail!("every CDN gave Epic's manifest for this game with another SHA-1 than Epic named")
        }
        Err(ManifestFetch::TooLarge) => anyhow::bail!("Epic's manifest for this game is larger than decanter reads"),
    };
    let (manifest, _, raw) = read_downloaded(raw).await?;
    anyhow::ensure!(
        !manifest.files().is_empty(),
        "Epic's manifest for this game lists no files"
    );
    let hashed = (std::sync::Arc::clone(&manifest), dest.clone());
    let report = tokio::task::spawn_blocking(move || hash_files([&*hashed.0], &hashed.1))
        .await
        .context("verifying the folder stopped")?;
    tracing::info!(
        account_id,
        app_name,
        build_version = manifest.build_version(),
        files_checked = report.files_checked,
        broken = report.broken.len(),
        "verified a folder to import as an epic game"
    );
    let mut imported = Imported::new(report);
    if !imported.broken.is_empty() {
        anyhow::ensure!(
            imported.broken.len() < imported.files_checked,
            "no file of {} is one of this game's",
            dest.display()
        );
        if differences == Differences::Refuse {
            return Ok(imported);
        }
    }
    let launch_executable = launch_program(&dest, None, manifest.launch_exe()).await;
    let record = InstalledGame {
        launch_executable,
        ..new_record(target, install_dir, manifest.build_version(), manifest.total_size())
    };
    save_record(library, &root, &record, &raw).await?;
    imported.is_recorded = true;
    tracing::info!(account_id, app_name, dest = %dest.display(), "imported an existing epic install");
    Ok(imported)
}
