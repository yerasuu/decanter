//! A Proton prefix: where it is, and where a program's stored Windows path
//! lands in it.

use std::path::{Path, PathBuf};

use crate::path::sanitize_relative_path;

/// A Proton prefix decanter owns.
///
/// umu treats `WINEPREFIX` as Proton's `STEAM_COMPAT_DATA_PATH` and then
/// symlinks `pfx` inside it back to the directory itself, so — unlike a
/// Steam `compatdata` layout — `drive_c` sits directly under this path.
#[derive(Debug, Clone)]
pub struct Prefix {
    /// Filesystem path to the prefix root.
    pub(crate) path: PathBuf,
}

impl Prefix {
    /// Where games get installed. Created on demand — umu creates it on
    /// first launch, but an install has to write files there before any
    /// launch has happened.
    #[inline]
    #[must_use]
    pub fn drive_c(&self) -> PathBuf {
        self.path.join("drive_c")
    }

    #[inline]
    pub fn new<P>(path: P) -> Self
    where
        P: Into<PathBuf>,
    {
        Self { path: path.into() }
    }

    #[inline]
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Resolves a Windows path as stored on a program (`C:\Games\x.exe`)
    /// to the host path umu should be handed.
    ///
    /// Only `C:` resolves: that's the only drive decanter ever creates a
    /// program under, and mapping an arbitrary drive letter would mean
    /// reading `dosdevices`, which is the prefix's business, not ours.
    #[inline]
    #[must_use]
    pub fn resolve_windows_path(&self, windows_path: &str) -> Option<PathBuf> {
        let Some(rest) = windows_path
            .strip_prefix("C:\\")
            .or_else(|| windows_path.strip_prefix("c:\\"))
        else {
            tracing::warn!(windows_path, "rejecting windows path: not on the C: drive");
            return None;
        };
        let Some(relative) = sanitize_relative_path(&rest.replace('\\', "/")) else {
            tracing::warn!(windows_path, "rejecting windows path: escapes the prefix");
            return None;
        };
        Some(self.drive_c().join(relative))
    }
}
