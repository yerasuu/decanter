//! Fetching and caching decanter's own copy of `umu-run`.
//!
//! Not resolved off `PATH` — confirmed the hard way: a `PATH`-resolved
//! `umu-run` living under `/nix/store/...` made pressure-vessel (the
//! Steam Linux Runtime container `umu-run` shells out to) fail its own
//! architecture-detection probe on a Nix-influenced host
//! ("pressure-vessel-wrap: W: Unable to determine architecture of
//! provider / ldconfig"), which took Vulkan down with it and crashed
//! every game a few seconds in. The exact same `umu-run`, copied to a
//! plain path outside `/nix/store` and run with an otherwise identical
//! environment, doesn't hit this at all — the bug is in how
//! pressure-vessel resolves *its own* deeply Nix-store-nested symlinks, a
//! known class of issue upstream (e.g. `ValveSoftware/steam-runtime#684`),
//! not anything an env var fixes.

use std::env;
use std::io;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, bail};
use decanter_core::runner::{Installed, Origin, Pin, Progress, Stage, Status, Tag};
use tokio::process::Command;

use crate::umu::{UMU_LAUNCHER_REPO, UMU_LAUNCHER_VERSION, umu_run_path};

/// Where the download in [`umu_update`] comes from — the portable
/// zipapp asset, not a distro package, since it has to run identically
/// regardless of what's installed system-wide.
fn umu_run_download_url() -> String {
    format!(
        "https://github.com/{UMU_LAUNCHER_REPO}/releases/download/\
         {UMU_LAUNCHER_VERSION}/umu-launcher-{UMU_LAUNCHER_VERSION}-zipapp.tar"
    )
}

/// What [`umu_update`] keeps one version of in the library's `bin` folder.
const FILE_PREFIX: &str = "umu-run-";

/// Every `umu-run` in the active library's `bin` folder, by version.
pub(crate) fn umu_status() -> Status {
    let Some(data_dir) = decanter_core::bootstrap::data_dir() else {
        return Status::Absent;
    };
    let Ok(entries) = std::fs::read_dir(data_dir.join("bin")) else {
        return Status::Absent;
    };
    let mut installed: Vec<Installed> = entries
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            let tag = entry
                .file_name()
                .to_str()
                .and_then(|name| name.strip_prefix(FILE_PREFIX))
                .and_then(Tag::try_new)?;
            path.is_file().then_some(Installed {
                pin: Pin::Release(tag),
                origin: Origin::Download,
                path,
            })
        })
        .collect();
    if installed.is_empty() {
        return Status::Absent;
    }
    installed.sort_by(|left, right| left.path.cmp(&right.path));
    Status::Installed(installed)
}

/// Installs `umu-run` at `pin` unless it is there; only [`UMU_LAUNCHER_VERSION`] is accepted, the one
/// decanter tested a real launch against.
pub(crate) async fn umu_prepare(
    pin: &Pin,
    on_progress: &mut (dyn FnMut(Progress) + Send),
) -> anyhow::Result<Installed> {
    let Pin::Release(tag) = pin;
    if tag.as_str() != UMU_LAUNCHER_VERSION {
        bail!("umu-run {tag} is not the tested {UMU_LAUNCHER_VERSION}");
    }
    let path = pinned_path()?;
    if !path.is_file() {
        install(&path, on_progress).await?;
    }
    Ok(Installed {
        pin: Pin::Release(tag.clone()),
        origin: Origin::Download,
        path,
    })
}

/// Downloads and extracts `umu-run` into [`umu_run_path`] the first time
/// it's needed, and just returns that path on every call after — this
/// module's own doc explains why decanter needs its own copy rather than
/// whatever `umu-run` happens to be on `PATH`. Only [`UMU_LAUNCHER_VERSION`]
/// changing invalidates the cached copy, and the copies of other versions
/// go once the new one is in; nothing here re-checks for updates on its own.
///
/// # Errors
/// Fails if the download, extraction, or making the result executable
/// fails.
pub(crate) async fn umu_update(on_progress: &mut (dyn FnMut(Progress) + Send)) -> anyhow::Result<Installed> {
    let Some(tag) = Tag::try_new(UMU_LAUNCHER_VERSION) else {
        bail!("umu-run's pinned version {UMU_LAUNCHER_VERSION:?} is no release name");
    };
    let was_installed = pinned_path()?.is_file();
    let installed = umu_prepare(&Pin::Release(tag), on_progress).await?;
    if !was_installed {
        remove_other_versions(&installed.path).await;
    }
    Ok(installed)
}

/// Removes `pin`'s copy from the active library.
pub(crate) async fn umu_remove(pin: &Pin) -> io::Result<()> {
    let Some(data_dir) = decanter_core::bootstrap::data_dir() else {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "the active library isn't available",
        ));
    };
    let Pin::Release(tag) = pin;
    let path = data_dir.join("bin").join(format!("{FILE_PREFIX}{tag}"));
    match tokio::fs::remove_file(&path).await {
        Ok(()) => {
            tracing::info!(path = %path.display(), "umu-run removed");
            Ok(())
        }
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err),
    }
}

fn pinned_path() -> anyhow::Result<PathBuf> {
    let Some(data_dir) = decanter_core::bootstrap::data_dir() else {
        bail!("the active library isn't available");
    };
    Ok(umu_run_path(&data_dir))
}

async fn install(path: &Path, on_progress: &mut (dyn FnMut(Progress) + Send)) -> anyhow::Result<()> {
    let url = umu_run_download_url();
    tracing::info!(url, "downloading umu-run");
    on_progress(Progress {
        stage: Stage::Downloading,
        received: 0,
        total: None,
    });
    let bytes = reqwest::get(&url)
        .await
        .context("requesting umu-run")?
        .error_for_status()
        .context("umu-run download returned an error status")?
        .bytes()
        .await
        .context("reading umu-run download body")?;
    let received = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
    on_progress(Progress {
        stage: Stage::Downloading,
        received,
        total: Some(received),
    });

    // A fresh temp dir per attempt rather than a fixed name: two decanter
    // processes racing to download at once (unlikely, but a headless CLI
    // command and the GUI both can) must not corrupt each other's
    // in-progress extraction.
    let temp_dir = env::temp_dir().join(format!("decanter-umu-run-{}", std::process::id()));
    tokio::fs::create_dir_all(&temp_dir)
        .await
        .context("creating temp dir for umu-run extraction")?;
    let archive_path = temp_dir.join("umu-launcher.tar");
    tokio::fs::write(&archive_path, &bytes)
        .await
        .context("writing downloaded umu-run archive")?;

    on_progress(Progress {
        stage: Stage::Unpacking,
        received: 0,
        total: None,
    });
    // Shelled out to rather than pulling in a tar-handling crate for one
    // archive this rarely: `tar` is as close to guaranteed present on
    // Linux as `umu-run` itself needing to be.
    let status = Command::new("tar")
        .arg("-xf")
        .arg(&archive_path)
        .arg("-C")
        .arg(&temp_dir)
        .status()
        .await
        .context("spawning tar to extract umu-run")?;
    if !status.success() {
        bail!("tar exited with {status} extracting umu-run");
    }

    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .context("creating the library's bin folder for umu-run")?;
    }
    tokio::fs::copy(temp_dir.join("umu").join("umu-run"), path)
        .await
        .context("installing the extracted umu-run")?;
    {
        use std::os::unix::fs::PermissionsExt as _;
        tokio::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))
            .await
            .context("making umu-run executable")?;
    }

    // Best-effort: a leftover temp dir costs disk space, not correctness.
    let _ = tokio::fs::remove_dir_all(&temp_dir).await;
    tracing::info!(path = %path.display(), "umu-run installed");
    Ok(())
}

async fn remove_other_versions(current: &Path) {
    let Some(bin) = current.parent() else {
        return;
    };
    let Ok(mut entries) = tokio::fs::read_dir(bin).await else {
        return;
    };
    while let Ok(Some(entry)) = entries.next_entry().await {
        let path = entry.path();
        let stale = path != current && entry.file_name().to_string_lossy().starts_with(FILE_PREFIX);
        if stale && let Err(err) = tokio::fs::remove_file(&path).await {
            tracing::warn!(path = %path.display(), %err, "cannot remove an old umu-run");
        }
    }
}
