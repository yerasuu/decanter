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
use std::path::{Path, PathBuf};

use anyhow::{Context as _, bail};
use tokio::process::Command;

/// The umu-launcher release [`ensure_umu_run`] downloads. Pinned and
/// bumped deliberately after testing a real launch against it, rather
/// than always fetching "latest" — a silent umu upgrade changing behavior
/// under decanter with no chance to verify it first is worse than lagging
/// a release behind.
pub const UMU_LAUNCHER_VERSION: &str = "1.4.4";

/// The GitHub repository whose releases [`UMU_LAUNCHER_VERSION`] names.
pub const UMU_LAUNCHER_REPO: &str = "Open-Wine-Components/umu-launcher";

/// Where the download in [`ensure_umu_run`] comes from — the portable
/// zipapp asset, not a distro package, since it has to run identically
/// regardless of what's installed system-wide.
fn umu_run_download_url() -> String {
    format!(
        "https://github.com/{UMU_LAUNCHER_REPO}/releases/download/\
         {UMU_LAUNCHER_VERSION}/umu-launcher-{UMU_LAUNCHER_VERSION}-zipapp.tar"
    )
}

/// Where decanter keeps its own copy of `umu-run` in a library's data
/// folder. Named by version, so a version bump downloads the new one.
#[must_use]
pub fn umu_run_path(data_dir: &Path) -> PathBuf {
    data_dir.join("bin").join(format!("umu-run-{UMU_LAUNCHER_VERSION}"))
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
pub(crate) async fn ensure_umu_run() -> anyhow::Result<PathBuf> {
    let Some(data_dir) = decanter_core::bootstrap::data_dir() else {
        bail!("the active library isn't available");
    };
    let path = umu_run_path(&data_dir);
    if path.is_file() {
        return Ok(path);
    }

    let url = umu_run_download_url();
    tracing::info!(url, "downloading umu-run");
    let bytes = reqwest::get(&url)
        .await
        .context("requesting umu-run")?
        .error_for_status()
        .context("umu-run download returned an error status")?
        .bytes()
        .await
        .context("reading umu-run download body")?;

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
    tokio::fs::copy(temp_dir.join("umu").join("umu-run"), &path)
        .await
        .context("installing the extracted umu-run")?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        tokio::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
            .await
            .context("making umu-run executable")?;
    }

    // Best-effort: a leftover temp dir costs disk space, not correctness.
    let _ = tokio::fs::remove_dir_all(&temp_dir).await;
    tracing::info!(path = %path.display(), "umu-run installed");
    remove_other_versions(&path).await;
    Ok(path)
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
        let stale = path != current && entry.file_name().to_string_lossy().starts_with("umu-run-");
        if stale && let Err(err) = tokio::fs::remove_file(&path).await {
            tracing::warn!(path = %path.display(), %err, "cannot remove an old umu-run");
        }
    }
}
