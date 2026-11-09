// SPDX-License-Identifier: GPL-3.0-or-later
//! Registering a game folder the user already has (stage S8 of docs/amazon-nile-integration-analysis.md,
//! section 4.3): the folder is hashed against Amazon's live manifest and, once it matches, recorded
//! as the install without moving or copying a file. The folder is untrusted, so it is held to the
//! shape a record needs and every file is reached through `disk::inside`, which refuses links.

use std::path::Path;

use anyhow::Context as _;
use decanter_core::Library;
pub use decanter_runtime::install::import::{Differences, Imported};
use decanter_runtime::install::import::{ImportFolder, resolve_folder};

use super::distribution::{self, Asked, ManifestFetch};
use super::{Target, hash_files, launch_program, new_record, save_record, staged_root};
use crate::library::is_id;
use crate::manifest::parse_signed_manifest;

/// Verifies `folder` against the live manifest of `target` and, when it matches (or `differences`
/// accepts that it does not), records it as the game's install where it stands.
///
/// `folder` must be a direct child of a library folder's `common/`, since an install record names
/// its folder by that single path component; one reached through a link is refused.
///
/// # Errors
///
/// When the game is installed or has a stopped download, the folder is not usable, Amazon cannot be
/// asked or answers a manifest that fails its checks, no file of the folder matches, or the record
/// can't be written.
#[inline]
pub async fn import(
    library: &Library,
    target: Target<'_>,
    folder: &Path,
    differences: Differences,
) -> anyhow::Result<Imported> {
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
    anyhow::ensure!(
        library
            .read_manifest(account_id, decanter_core::Storefront::AmazonGames, product_id)
            .is_none(),
        "this Amazon game is already installed"
    );
    anyhow::ensure!(
        staged_root(library, account_id, product_id).is_none(),
        "this Amazon game has a stopped download: resume or cancel it first"
    );
    let ImportFolder {
        root,
        install_dir,
        dest,
        ..
    } = resolve_folder(library, folder)?;
    anyhow::ensure!(
        !library.install_dir_is_shared(&install_dir, account_id, product_id),
        "another game already uses the folder {install_dir:?}"
    );

    let download = match distribution::game_download(account_id, entitlement_id).await? {
        Asked::Ready(download) => download,
        Asked::SessionClosed => anyhow::bail!("Amazon closed the session: log in to Amazon Games again in Settings"),
        Asked::Unreachable(reason) => anyhow::bail!("Amazon could not be reached: {reason}"),
    };
    let raw = match distribution::manifest(&download).await {
        Ok(raw) => raw,
        Err(ManifestFetch::Unreachable(reason)) => anyhow::bail!("Amazon's manifest could not be fetched: {reason}"),
        Err(ManifestFetch::TooLarge) => anyhow::bail!("Amazon's manifest for this game is larger than decanter reads"),
    };
    let manifest = parse_signed_manifest(&raw).context("Amazon's manifest for this game was refused")?;
    anyhow::ensure!(
        !manifest.files().is_empty(),
        "Amazon's manifest for this game lists no files"
    );
    let total_size = manifest.total_size();
    let hashed = dest.clone();
    let report = tokio::task::spawn_blocking(move || hash_files(&manifest, &hashed))
        .await
        .context("verifying the folder stopped")?;
    tracing::info!(
        account_id,
        product_id,
        files_checked = report.files_checked,
        broken = report.broken.len(),
        "verified a folder to import as an amazon game"
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
    let launch_executable = launch_program(&dest, None).await;
    let record = decanter_core::InstalledGame {
        launch_executable,
        ..new_record(target, install_dir, &download, total_size)
    };
    save_record(library, &root, &record, &raw).await?;
    imported.is_recorded = true;
    tracing::info!(account_id, product_id, dest = %dest.display(), "imported an existing amazon install");
    Ok(imported)
}
