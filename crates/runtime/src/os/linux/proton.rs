//! Handing umu the Proton build a launch runs against, and where Steam keeps
//! the ones it has installed.

use std::env;
use std::ffi::OsStr;
use std::fs;
use std::path::PathBuf;

use decanter_core::runner::{Installed, Origin, Pin, Status, Tag};

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

/// Steam's own folder for them, under the XDG data directory.
pub(crate) fn steam_compat_dir() -> Option<PathBuf> {
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

/// Every Proton build installed for Steam.
///
/// This is the list a prefix picks its runner from: Proton-GE, Proton-EM,
/// `CachyOS`'s builds, whatever the user installed. umu can also download
/// its own (see [`Proton::Latest`]), but a build that is already on disk
/// costs nothing and is the one the user chose.
pub(crate) fn proton_status() -> Status {
    let Some(directory) = steam_compat_dir() else {
        return Status::Absent;
    };
    let Ok(entries) = fs::read_dir(&directory) else {
        tracing::debug!(directory = %directory.display(), "no steam compat dir to scan, no runners found");
        return Status::Absent;
    };
    let mut runners: Vec<Installed> = entries
        .flatten()
        .filter_map(|entry| {
            // `proton` is the launcher script every Proton build ships;
            // without it this is a leftover directory, not a runner.
            let path = entry.path();
            let is_runner = path.join("proton").is_file();
            tracing::trace!(candidate = %path.display(), is_runner, "checking proton build candidate");
            let tag = is_runner
                .then(|| entry.file_name().to_str().and_then(Tag::try_new))
                .flatten()?;
            Some(Installed {
                pin: Pin::Release(tag),
                origin: Origin::Download,
                path,
            })
        })
        .collect();
    runners.sort_by(|left, right| left.path.cmp(&right.path));
    tracing::debug!(?runners, "resolved installed proton builds");
    if runners.is_empty() {
        return Status::Absent;
    }
    Status::Installed(runners)
}
