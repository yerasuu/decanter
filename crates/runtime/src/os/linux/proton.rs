//! Handing umu the Proton build a launch runs against, and finding the ones
//! Steam already has installed.

use std::env;
use std::ffi::OsStr;
use std::fs;
use std::path::PathBuf;

use crate::Proton;

impl Proton {
    /// Value for umu's `PROTONPATH` env var, or `None` to leave it unset.
    pub(crate) fn as_env_value(&self) -> Option<&OsStr> {
        match self {
            // Unset, not empty: umu treats an empty PROTONPATH as "choose
            // for me", and an absent one the same way.
            Self::Latest => None,
            Self::Named(name) => Some(OsStr::new(name)),
            Self::Path(path) => Some(path.as_os_str()),
        }
    }
}

/// Where umu downloads [`Proton::Latest`] from, into [`steam_compat_dir`]
/// under the release's tag.
pub const UMU_PROTON_REPO: &str = "Open-Wine-Components/umu-proton";

/// Where Steam (and umu) keep Proton builds.
///
/// umu resolves a bare `PROTONPATH` against this directory, so a build
/// that lives here can be named rather than pointed at by full path —
/// which is what [`Proton::Named`] carries.
#[inline]
#[must_use]
pub fn steam_compat_dir() -> Option<PathBuf> {
    if !crate::available() {
        return None;
    }
    let Some(data) = env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/share")))
    else {
        tracing::warn!("neither XDG_DATA_HOME nor HOME is set, cannot resolve the Steam compat dir");
        return None;
    };
    Some(data.join("Steam").join("compatibilitytools.d"))
}

/// Every Proton build installed for Steam, newest-looking last — sorted by
/// name, which is as close to a version order as these get.
///
/// This is the list a prefix picks its runner from: Proton-GE, Proton-EM,
/// `CachyOS`'s builds, whatever the user installed. umu can also download
/// its own (see [`Proton::Latest`]), but a build that is already on disk
/// costs nothing and is the one the user chose.
#[inline]
#[must_use]
pub fn steam_runners() -> Vec<String> {
    let Some(directory) = steam_compat_dir() else {
        return Vec::new();
    };
    let Ok(entries) = fs::read_dir(&directory) else {
        tracing::debug!(directory = %directory.display(), "no steam compat dir to scan, no runners found");
        return Vec::new();
    };
    let mut runners: Vec<String> = entries
        .flatten()
        .filter_map(|entry| {
            // `proton` is the launcher script every Proton build ships;
            // without it this is a leftover directory, not a runner.
            let path = entry.path();
            let is_runner = path.join("proton").is_file();
            tracing::trace!(candidate = %path.display(), is_runner, "checking proton build candidate");
            is_runner
                .then(|| entry.file_name().to_str().map(str::to_owned))
                .flatten()
        })
        .collect();
    runners.sort();
    tracing::debug!(?runners, "resolved installed proton builds");
    runners
}

/// Whether `name` is a Proton build installed for Steam. Used to check a
/// stored choice still exists before launching against it.
#[inline]
#[must_use]
pub fn steam_runner_exists(name: &str) -> bool {
    let exists = steam_compat_dir().is_some_and(|directory| directory.join(name).join("proton").is_file());
    tracing::debug!(name, exists, "checked whether a named proton build is installed");
    exists
}
