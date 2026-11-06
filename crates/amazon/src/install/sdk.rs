// SPDX-License-Identifier: GPL-3.0-or-later
//! Amazon's own Games SDK, fetched from Amazon's CDN into the library rather than shipped with
//! decanter (stage S5 of docs/amazon-nile-integration-analysis.md, section 5.2): the files of the
//! launcher's update channel whose path holds `FuelSDK_x64.dll` or `AmazonGamesSDK_`, through the
//! same signed manifest, host allow-list and hashed transfer as a game's files. What games call in
//! it is UNKNOWN; nile only points `FUEL_DIR` and `AMAZON_GAMES_SDK_PATH` at it.
//!
//! ```text
//! <root>/decanter/amazon/sdk/<path>        the SDK, laid out as the manifest names it
//! <root>/decanter/amazon/sdk.version       written once all of it is in place
//! <root>/decanter/amazon/sdk-downloading/  staging, as a game's
//! ```

use core::sync::atomic::{AtomicU64, AtomicUsize};
use std::collections::HashSet;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::Context as _;
use tokio::sync::Mutex as AsyncMutex;

use super::distribution::{self, ManifestFetch};
use super::fetch::{self, Failure, Job, Run};
use super::stop::StopFlag;
use super::{FILES, disk};
use crate::manifest::{Manifest, parse_signed_manifest};

/// nile's test for an SDK file among the launcher's (analysis, section 5.2).
const MARKS: [&str; 2] = ["FuelSDK_x64.dll", "AmazonGamesSDK_"];

/// The SDK's two folders, which nile points `FUEL_DIR` and `AMAZON_GAMES_SDK_PATH` at.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct SdkDirs {
    pub fuel_dir: PathBuf,
    pub sdk_path: PathBuf,
}

impl SdkDirs {
    fn under(root: &Path) -> Self {
        let services = disk::sdk_dir(root).join("Amazon Games Services");
        Self {
            fuel_dir: services.join("Legacy"),
            sdk_path: services.join("AmazonGamesSDK"),
        }
    }

    fn are_present(&self) -> bool {
        self.fuel_dir.is_dir() && self.sdk_path.is_dir()
    }
}

/// The SDK in the library folder `root`, fetched from Amazon first when it is not there yet. Once
/// it is, nothing is asked of Amazon again.
///
/// # Errors
///
/// Amazon or its CDN not answering, a manifest that fails its checks or lists no SDK file, a file
/// that kept arriving corrupted, a disk error.
#[inline]
pub async fn ensure_sdk(root: &Path) -> anyhow::Result<SdkDirs> {
    let dirs = SdkDirs::under(root);
    let marker = disk::sdk_marker(root);
    if marker.is_file() && dirs.are_present() {
        return Ok(dirs);
    }
    let staging = disk::sdk_staging(root);
    tokio::fs::create_dir_all(&staging)
        .await
        .with_context(|| format!("creating {}", staging.display()))?;
    let held = super::lock_staging(&staging).await?;
    // Another game's launch may have fetched it while this one waited for the lock.
    if marker.is_file() && dirs.are_present() {
        return Ok(dirs);
    }
    tracing::info!(root = %root.display(), "fetching the amazon games sdk");
    let download = distribution::sdk_download().await?;
    let raw = distribution::manifest(&download).await.map_err(|err| match err {
        ManifestFetch::Unreachable(reason) => anyhow::anyhow!("fetching the SDK's manifest: {reason}"),
        ManifestFetch::TooLarge => anyhow::anyhow!("the SDK's manifest is larger than decanter reads"),
    })?;
    let manifest = Arc::new(parse_signed_manifest(&raw).context("Amazon's SDK manifest was refused")?);
    let wanted: Vec<usize> = manifest
        .files()
        .iter()
        .enumerate()
        .filter(|&(_, file)| MARKS.iter().any(|mark| file.path().as_str().contains(mark)))
        .map(|(index, _)| index)
        .collect();
    anyhow::ensure!(!wanted.is_empty(), "the launcher's manifest lists no SDK file");

    let sdk = disk::sdk_dir(root);
    let files_dir = staging.join(FILES);
    let (log, done) = super::open_log(&staging).await?;
    let sorted = {
        let (manifest, sdk, files_dir) = (Arc::clone(&manifest), sdk.clone(), files_dir.clone());
        tokio::task::spawn_blocking(move || sort(&manifest, &wanted, &sdk, &files_dir, &done))
            .await
            .context("checking the SDK files on disk stopped")?
            .context("checking the SDK files already on disk")?
    };
    let jobs: Vec<Job<'_>> = sorted
        .fetch
        .iter()
        .filter_map(|&index| {
            let file = manifest.files().get(index)?;
            Some(Job {
                index,
                file,
                done: files_dir.join(file.path().as_path()),
            })
        })
        .collect();
    let on_disk: Vec<AtomicU64> = jobs.iter().map(|_| AtomicU64::new(0)).collect();
    let log = AsyncMutex::new(log);
    let run = Run {
        download: &download,
        flag: &StopFlag::default(),
        log: &log,
        on_disk: &on_disk,
        current: &AtomicUsize::new(usize::MAX),
    };
    fetch::fetch_all(&run, &jobs, || {})
        .await
        .map_err(|failure| match failure {
            Failure::Stopped | Failure::Unreachable => anyhow::anyhow!("Amazon's CDN did not deliver the SDK"),
            Failure::DiskFull => anyhow::anyhow!("the disk is full"),
            Failure::Corrupt { path } => anyhow::anyhow!("{path} kept arriving from Amazon with the wrong hash"),
            Failure::Io { path, source } => anyhow::Error::new(source).context(format!("writing {path} into staging")),
        })?;

    let moved: Vec<usize> = sorted.staged.iter().chain(&sorted.fetch).copied().collect();
    let landing = {
        let (manifest, sdk) = (Arc::clone(&manifest), sdk.clone());
        tokio::task::spawn_blocking(move || land(&manifest, &moved, &sdk, &files_dir))
    };
    landing
        .await
        .context("moving the SDK into the library stopped")?
        .with_context(|| format!("moving the SDK into {}", sdk.display()))?;
    decanter_runtime::journal::write_atomic(&marker, download.version_id.as_bytes())
        .await
        .with_context(|| format!("writing {}", marker.display()))?;
    drop(held);
    if let Err(err) = tokio::fs::remove_dir_all(&staging).await {
        tracing::warn!(staging = %staging.display(), error = %err, "couldn't remove the finished amazon sdk staging");
    }
    tracing::info!(
        version_id = download.version_id,
        files = sorted.staged.len().saturating_add(sorted.fetch.len()),
        sdk = %sdk.display(),
        "amazon games sdk in place"
    );
    if !dirs.are_present() {
        tracing::warn!(
            fuel_dir = %dirs.fuel_dir.display(),
            sdk_path = %dirs.sdk_path.display(),
            "the amazon sdk landed without the folders nile points the games at"
        );
    }
    Ok(dirs)
}

/// The SDK files still to fetch, and those already finished in staging; the rest are in place.
struct Sorted {
    fetch: Vec<usize>,
    staged: Vec<usize>,
}

fn sort(
    manifest: &Manifest,
    wanted: &[usize],
    sdk: &Path,
    files_dir: &Path,
    done: &HashSet<String>,
) -> io::Result<Sorted> {
    let mut sorted = Sorted {
        fetch: Vec::new(),
        staged: Vec::new(),
    };
    for &index in wanted {
        let Some(file) = manifest.files().get(index) else {
            continue;
        };
        let staged = disk::inside(files_dir, file.path())?;
        let is_staged = if done.contains(&fetch::done_record(file)) {
            disk::has_size(&staged, file.size())?
        } else {
            disk::matches(&staged, file)?
        };
        if is_staged {
            sorted.staged.push(index);
            continue;
        }
        if !disk::matches(&disk::inside(sdk, file.path())?, file)? {
            sorted.fetch.push(index);
        }
    }
    Ok(sorted)
}

fn land(manifest: &Manifest, moved: &[usize], sdk: &Path, files_dir: &Path) -> anyhow::Result<()> {
    for file in moved.iter().filter_map(|&index| manifest.files().get(index)) {
        let staged = disk::inside(files_dir, file.path())?;
        let destination = disk::inside(sdk, file.path())?;
        disk::move_into_place(&staged, &destination)
            .with_context(|| format!("moving {} into the SDK folder", file.path().as_str()))?;
    }
    Ok(())
}
