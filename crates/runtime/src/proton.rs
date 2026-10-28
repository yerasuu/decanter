//! Which Proton build a launch runs against, and where Steam keeps the ones
//! it has installed ([`crate::runner::status`] lists them).

use std::path::PathBuf;

use crate::os;

/// Which Proton build to run against.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub enum Proton {
    /// Let umu pick and download the current build. What a prefix with no
    /// explicit choice gets.
    #[default]
    Latest,
    /// A build installed for Steam (see [`crate::runner::status`]), or one of
    /// umu's own version keywords (`GE-Proton`, `UMU-Proton`, ...).
    /// Passed through untouched: umu resolves a bare name against
    /// [`steam_compat_dir`] first, and treats what's left as a keyword to
    /// download.
    Named(String),
    /// A Proton directory on disk, used as-is.
    Path(PathBuf),
}

/// Where umu downloads [`Proton::Latest`] from, into [`steam_compat_dir`]
/// under the release's tag.
pub const UMU_PROTON_REPO: &str = "Open-Wine-Components/umu-proton";

/// Where Steam (and umu) keep Proton builds; `None` off Linux, where
/// nothing runs under Proton.
///
/// umu resolves a bare `PROTONPATH` against this directory, so a build
/// that lives here can be named rather than pointed at by full path —
/// which is what [`Proton::Named`] carries.
#[inline]
#[must_use]
pub fn steam_compat_dir() -> Option<PathBuf> {
    os::steam_compat_dir()
}
