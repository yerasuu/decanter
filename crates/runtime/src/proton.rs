//! Which Proton build a launch runs against.

use std::path::PathBuf;

/// Which Proton build to run against.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub enum Proton {
    /// Let umu pick and download the current build. What a prefix with no
    /// explicit choice gets.
    #[default]
    Latest,
    /// A build installed for Steam (see `steam_runners`), or one of
    /// umu's own version keywords (`GE-Proton`, `UMU-Proton`, ...).
    /// Passed through untouched: umu resolves a bare name against
    /// `steam_compat_dir` first, and treats what's left as a keyword to
    /// download.
    Named(String),
    /// A Proton directory on disk, used as-is.
    Path(PathBuf),
}
