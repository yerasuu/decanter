//! Checking installed files against the depot manifest that put them
//! there, and fixing whatever doesn't match.
//!
//! Verification touches only the network for the manifest itself — every
//! byte comparison happens against the local disk. A depot chunk carries
//! its own uncompressed size and MD5 ([`crate::api::client::DepotChunk`]), so a
//! file is read in exactly those slices, in order, and each slice hashed
//! and compared. No re-download, no CDN chunk URLs, until something
//! actually needs fixing.

use std::borrow::Cow;
use std::collections::HashMap;
use std::path::Path;

use anyhow::Context;
use decanter_core::{Storefront, platform};

use super::download::PlannedDownload;
use super::plan::{manifest_relative_path, resolve_depot_files};
use crate::api::client::{DepotChunk, Sessionless};
use crate::hook::REAL_DLL_BACKUP as HOOKED_GALAXY_DLL_BACKUP;

/// One file that didn't match its manifest.
#[derive(Debug, Clone)]
pub struct BrokenFile {
    pub relative_path: String,
    /// Whose depot this file's chunks belong to — the base game's own id,
    /// or a DLC's. [`repair`] needs this to fetch from the right depot;
    /// two products can each list a file at the same path, and repairing
    /// the wrong one's copy would silently fetch bytes for a DLC that
    /// isn't installed.
    pub owner: String,
    pub reason: BrokenReason,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BrokenReason {
    Missing,
    /// On disk, but at least one chunk's hash didn't match — a partial
    /// write, disk corruption, or a hand-edited file.
    Mismatched,
}

/// What verifying one game (and its installed DLC) found.
#[derive(Debug, Clone, Default)]
pub struct VerifyReport {
    pub files_checked: usize,
    pub broken: Vec<BrokenFile>,
}

impl VerifyReport {
    pub fn is_clean(&self) -> bool {
        self.broken.is_empty()
    }
}

/// Verifies every file the game's build lists, and every file each
/// installed DLC's own depot lists, against what's actually on disk.
///
/// Requires a live GOG session: the depot manifest is fetched fresh
/// (`build_owner`'s current build), not read from what was recorded at
/// install time — a manifest for a build GOG has since retired can 404,
/// which is reported as part of the outcome rather than failing the whole
/// verification silently.
pub async fn verify(
    library: &decanter_core::Library,
    account_id: &str,
    product_id: &str,
) -> anyhow::Result<VerifyReport> {
    let game = crate::achv_try!(
        library
            .read_manifest(account_id, Storefront::Gog, product_id)
            .with_context(|| format!("{product_id} is not installed"))
    );
    let install_root = crate::achv_try!(
        library
            .install_dir(&game.install_dir)
            .context("unusable install directory")
    );
    tracing::info!(product_id, install_dir = game.install_dir, "starting verify");

    let game_id: i64 = crate::achv_try!(product_id.parse().context("GOG product_id must be numeric"));
    let access_token = crate::achv_try!(
        crate::session::live_token(account_id)
            .await
            .map(|token| token.access_token)
    );
    let http = crate::achv_try!(Sessionless::new().context("building the GOG HTTP client"));

    let branch = game.branch.as_deref();
    // Pinned to the build actually on disk, not whatever the branch
    // currently points at -- a game rolled back to an older build (or one
    // that just hasn't picked up a newer one yet) would otherwise get
    // diffed against a manifest for files that were never installed, and
    // `repair` would "fix" it by silently upgrading it out from under the
    // pin.
    let build_id = Some(game.build_id.as_str());
    let mut report = VerifyReport::default();
    crate::achv_try!(
        verify_content(
            &http,
            &access_token,
            game_id,
            game_id,
            branch,
            build_id,
            &install_root,
            &mut report,
        )
        .await
    );

    for dlc_id in game.dlcs.keys() {
        let dlc_id: i64 = crate::achv_try!(dlc_id.parse().context("GOG DLC id must be numeric"));
        crate::achv_try!(
            verify_content(
                &http,
                &access_token,
                game_id,
                dlc_id,
                branch,
                build_id,
                &install_root,
                &mut report,
            )
            .await
        );
    }

    tracing::info!(
        product_id,
        files_checked = report.files_checked,
        broken_count = report.broken.len(),
        "verify report finished"
    );
    Ok(report)
}

async fn verify_content(
    http: &Sessionless,
    access_token: &str,
    build_owner: i64,
    content: i64,
    branch: Option<&str>,
    build_id: Option<&str>,
    install_root: &Path,
    report: &mut VerifyReport,
) -> anyhow::Result<()> {
    let (_, _, files) = crate::achv_try!(
        resolve_depot_files(
            http,
            access_token,
            build_owner,
            content,
            branch,
            build_id,
            platform::game_system()
        )
        .await
        .inspect_err(|err| tracing::error!(content, error = %err, "depot manifest fetch failed, aborting verify"))
        .with_context(|| format!("fetching the depot manifest for {content}"))
    );

    // Hashing every installed file is real disk I/O over potentially
    // gigabytes — run it on the blocking pool rather than inline in this
    // async fn, where it would otherwise stall the tokio worker thread for
    // the length of a full-library verify.
    let install_root = install_root.to_path_buf();
    let (checked, broken) = crate::achv_try!(
        tokio::task::spawn_blocking(move || {
            let mut checked = 0_usize;
            let mut broken = Vec::new();
            for file in files {
                let Some(relative_path) = manifest_relative_path(&file.path) else {
                    continue;
                };
                let path = install_root.join(&relative_path);
                checked += 1;

                let reason = match verify_file(&verification_target(&path), &file.chunks) {
                    Ok(true) => {
                        tracing::debug!(relative_path = %relative_path.display(), "file verified ok");
                        continue;
                    }
                    Err(err) if err.kind() == std::io::ErrorKind::NotFound => BrokenReason::Missing,
                    // A mismatched hash and any other I/O error (permissions, a
                    // directory where a file should be) are both "broken" —
                    // only a missing file gets its own reason.
                    Ok(false) | Err(_) => BrokenReason::Mismatched,
                };
                tracing::debug!(relative_path = %relative_path.display(), ?reason, "file verification found a problem");
                broken.push(BrokenFile {
                    relative_path: relative_path.to_string_lossy().into_owned(),
                    owner: content.to_string(),
                    reason,
                });
            }
            (checked, broken)
        })
        .await
        .context("verify worker task panicked")
    );
    report.files_checked += checked;
    report.broken.extend(broken);
    Ok(())
}

/// Where to actually read `path`'s bytes from for verification.
///
/// `Galaxy64.dll` is the one manifest file this pipeline installs and
/// then lets something else legitimately replace: decanter's own
/// achievement hook swaps in a proxy under that exact name and renames
/// the real DLL to [`HOOKED_GALAXY_DLL_BACKUP`] beside it. Without this
/// redirect, every verify after the hook installs would report a
/// permanent, "unrepairable" mismatch on a file that is deliberately not
/// what the manifest describes — and `repair` would silently overwrite
/// decanter's own hook with the genuine DLL, undoing it. The renamed copy
/// is what still has to match the manifest; if it isn't there, `path` is
/// checked as normal, which also covers a game the hook was never
/// installed for.
fn verification_target(path: &Path) -> Cow<'_, Path> {
    let is_galaxy_dll = path
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.eq_ignore_ascii_case("Galaxy64.dll"));
    if !is_galaxy_dll {
        return Cow::Borrowed(path);
    }
    let Some(backup) = path.parent().map(|parent| parent.join(HOOKED_GALAXY_DLL_BACKUP)) else {
        return Cow::Borrowed(path);
    };
    if backup.is_file() {
        Cow::Owned(backup)
    } else {
        Cow::Borrowed(path)
    }
}

/// Reads `path` in the manifest's own chunk boundaries and hashes each
/// slice. `Ok(false)` for a hash mismatch, `Err` for a file that couldn't
/// even be read (missing, wrong size, permissions).
fn verify_file(path: &Path, chunks: &[DepotChunk]) -> std::io::Result<bool> {
    use std::io::Read;
    let mut file = crate::achv_try!(std::fs::File::open(path));
    for chunk in chunks {
        let mut buffer = vec![0u8; chunk.size as usize];
        // A file shorter than the manifest expects is wrong the same way a
        // bad hash is — it isn't "file not found". Only an actually absent
        // path should ever surface as `BrokenReason::Missing`.
        if let Err(err) = file.read_exact(&mut buffer) {
            return if err.kind() == std::io::ErrorKind::UnexpectedEof {
                Ok(false)
            } else {
                Err(err)
            };
        }
        let digest = format!("{:x}", md5::compute(&buffer));
        if digest != chunk.md5.to_ascii_lowercase() {
            return Ok(false);
        }
    }
    // A file with extra trailing bytes beyond every listed chunk is as
    // wrong as one with a bad hash — it isn't what the manifest describes.
    let mut trailing = [0u8; 1];
    match file.read(&mut trailing) {
        Ok(0) => Ok(true),
        Ok(_) => Ok(false),
        Err(err) => Err(err),
    }
}

/// Re-downloads exactly the files [`verify`] flagged, grouped by which
/// depot they came from, and drops them back into place.
///
/// Runs the same plan → download → move pipeline [`super::install`] uses,
/// just filtered down to the broken set — a repair is a small install, not
/// a special case of one. So it reports through `on_progress` and stops on
/// [`super::cancel`] for the same `(account_id, product_id)` the same way,
/// failing with [`super::Cancelled`].
pub async fn repair(
    library: &decanter_core::Library,
    account_id: &str,
    product_id: &str,
    broken: &[BrokenFile],
    mut on_progress: impl FnMut(super::Progress) + Send + 'static,
) -> anyhow::Result<()> {
    if broken.is_empty() {
        return Ok(());
    }
    tracing::info!(product_id, broken_count = broken.len(), "starting repair");
    super::tracking::claim(account_id, product_id).await;
    let key = (account_id.to_owned(), product_id.to_owned());
    let game = crate::achv_try!(
        library
            .read_manifest(account_id, Storefront::Gog, product_id)
            .with_context(|| format!("{product_id} is not installed"))
    );
    let install_root = crate::achv_try!(
        library
            .install_dir(&game.install_dir)
            .context("unusable install directory")
    );
    let game_id: i64 = crate::achv_try!(product_id.parse().context("GOG product_id must be numeric"));

    let access_token = crate::achv_try!(
        crate::session::live_token(account_id)
            .await
            .map(|token| token.access_token)
    );

    let staging = crate::achv_try!(super::staging::staging_root(
        &install_root,
        account_id,
        &format!("{product_id}-repair")
    ));
    if let Err(err) = tokio::fs::remove_dir_all(&staging).await {
        if err.kind() != std::io::ErrorKind::NotFound {
            tracing::warn!(path = %staging.display(), error = %err, "couldn't clear stale repair staging directory");
        }
    }

    let (downloads, total_size) = crate::achv_try!(plan_repair(&game, &access_token, game_id, broken).await);

    if super::tracking::cancelled_while_queued(&key).await {
        tracing::info!(product_id, "repair cancelled before download started");
        return Err(super::Cancelled.into());
    }
    if downloads.is_empty() {
        tracing::warn!(
            product_id,
            "no broken file is listed by the build any more, nothing to repair"
        );
        return Ok(());
    }
    crate::achv_try!(
        super::download_into_staging(downloads, Some(total_size), &staging, key, product_id, &mut on_progress)
            .await
            .inspect_err(|err| {
                if !err.is::<super::Cancelled>() {
                    tracing::error!(product_id, error = %err, "repair download failed, aborting repair");
                }
            })
    );

    crate::achv_try!(
        super::staging::move_into_library(&staging, &install_root)
            .await
            .inspect_err(|err| tracing::error!(product_id, error = %err, "moving repaired files into place failed"))
            .context("moving repaired files into place")
    );
    if let Err(err) = tokio::fs::remove_dir_all(&staging).await {
        if err.kind() != std::io::ErrorKind::NotFound {
            tracing::warn!(path = %staging.display(), error = %err, "couldn't remove repair staging directory");
        }
    }
    tracing::info!(product_id, "repair completed");
    Ok(())
}

/// Every broken file's download, planned per owner before any byte moves,
/// so the progress total covers the whole repair and one cancel flag stops
/// all of it. Also returns the bytes to download.
async fn plan_repair(
    game: &decanter_core::InstalledGame,
    access_token: &str,
    game_id: i64,
    broken: &[BrokenFile],
) -> anyhow::Result<(Vec<PlannedDownload>, u64)> {
    let mut by_owner: HashMap<&str, Vec<&str>> = HashMap::new();
    for file in broken {
        by_owner
            .entry(file.owner.as_str())
            .or_default()
            .push(file.relative_path.as_str());
    }

    let system = platform::game_system();
    let bundle_program = super::program::bundle_program(game, system);
    let mut downloads = Vec::with_capacity(broken.len());
    let mut total_size = 0u64;
    for (owner, wanted) in by_owner {
        let owner_id: i64 = crate::achv_try!(owner.parse().context("GOG product id must be numeric"));
        // Same build the game is actually on, not the branch's latest --
        // see `verify`'s own note. A repair has to put back what was
        // there, not silently upgrade the game while fixing it.
        let plan = crate::achv_try!(
            super::plan::plan(
                access_token,
                game_id,
                owner_id,
                game.branch.as_deref(),
                Some(game.build_id.as_str()),
                system,
            )
            .await
        );
        for file in plan
            .files
            .into_iter()
            .filter(|file| wanted.contains(&file.relative_path.as_str()))
        {
            total_size += file.size;
            let is_executable = file.is_executable || bundle_program == Some(file.relative_path.as_str());
            downloads.push(PlannedDownload {
                relative_path: file.relative_path,
                chunks: file.chunks,
                is_executable,
            });
        }
    }
    Ok((downloads, total_size))
}
