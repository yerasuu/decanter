//! Which umu-launcher release decanter runs games under Proton with, and
//! where a library keeps its copy.

use std::path::{Path, PathBuf};

/// The umu-launcher release decanter downloads for itself on Linux. Pinned and
/// bumped deliberately after testing a real launch against it, rather
/// than always fetching "latest" — a silent umu upgrade changing behavior
/// under decanter with no chance to verify it first is worse than lagging
/// a release behind.
pub const UMU_LAUNCHER_VERSION: &str = "1.4.4";

/// The GitHub repository whose releases [`UMU_LAUNCHER_VERSION`] names.
pub const UMU_LAUNCHER_REPO: &str = "Open-Wine-Components/umu-launcher";

/// Where decanter keeps its own copy of `umu-run` in a library's data
/// folder. Named by version, so a version bump downloads the new one.
#[must_use]
pub fn umu_run_path(data_dir: &Path) -> PathBuf {
    data_dir.join("bin").join(format!("umu-run-{UMU_LAUNCHER_VERSION}"))
}
