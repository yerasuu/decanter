//! Which umu-launcher release decanter runs games under Proton with, and
//! where a library keeps its copy.

use std::io;
use std::path::{Path, PathBuf};

/// The umu-launcher release decanter downloads for itself on Linux. Pinned and
/// bumped deliberately after testing a real launch against it, rather
/// than always fetching "latest" — a silent umu upgrade changing behavior
/// under decanter with no chance to verify it first is worse than lagging
/// a release behind.
pub const UMU_LAUNCHER_VERSION: &str = "1.4.4";

/// Lower-case hex SHA-256 of [`UMU_LAUNCHER_VERSION`]'s `umu-launcher-<version>-zipapp.tar`, which
/// its download must match: the asset's digest in GitHub's releases API. Until it is filled in,
/// every download of `umu-run` refuses.
pub const UMU_LAUNCHER_SHA256: &str = "";

/// The GitHub repository whose releases [`UMU_LAUNCHER_VERSION`] names.
pub const UMU_LAUNCHER_REPO: &str = "Open-Wine-Components/umu-launcher";

/// A folder holding only `umu-run`, as a link to `installed` (the copy
/// [`crate::runner::prepare`] returned for [`Runner::Umu`]), to put first on
/// `PATH` for a tool that looks for `umu-run` there, such as psport's FSR
/// 4.1.1 recording: decanter's copy is named by its version.
///
/// [`Runner::Umu`]: decanter_core::runner::Runner::Umu
///
/// # Errors
/// Fails off Linux, or when the folder or the link can't be made.
#[inline]
pub async fn umu_run_dir(installed: &Path) -> io::Result<PathBuf> {
    crate::os::umu_on_path(installed).await
}

/// Where decanter keeps its own copy of `umu-run` in a library's data
/// folder. Named by version, so a version bump downloads the new one.
#[must_use]
pub fn umu_run_path(data_dir: &Path) -> PathBuf {
    data_dir.join("bin").join(format!("umu-run-{UMU_LAUNCHER_VERSION}"))
}
