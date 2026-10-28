//! Handing umu the Proton build a launch runs against, and where Steam keeps
//! the ones it has installed.

use std::env;
use std::ffi::OsStr;
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
