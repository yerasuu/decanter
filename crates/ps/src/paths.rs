//! Where decanter keeps everything PS: a `psport` folder of its own inside
//! the active profile's library root, beside `common/` and `manifests/`.
//! It lives on the same disk as the PC games and moves with the library;
//! `decanter_core::Library` only ever reads the folders it knows by name,
//! so its scans and the Steam export never see this one.

use std::path::{Path, PathBuf};
use std::sync::RwLock;

/// Set by [`use_library`] whenever the active profile is known; read on
/// every path, written once per profile switch.
static ROOT: RwLock<Option<PathBuf>> = RwLock::new(None);

/// Before a profile is active: the default library's folder, where a
/// profile that never chose its own library keeps it too.
fn default_root() -> PathBuf {
    decanter_core::Library::default_path()
        .unwrap_or_else(|| decanter_core::data_dir().join("steamapps"))
        .join("psport")
}

pub fn root() -> PathBuf {
    match ROOT.read() {
        Ok(root) => root.clone().unwrap_or_else(default_root),
        Err(poisoned) => poisoned.into_inner().clone().unwrap_or_else(default_root),
    }
}

/// Points every path at `library_root`'s `psport` folder.
pub fn use_library(library_root: &Path) {
    let root = library_root.join("psport");
    match ROOT.write() {
        Ok(mut current) => *current = Some(root),
        Err(poisoned) => *poisoned.into_inner() = Some(root),
    }
}

/// The only folder PS games are read from.
pub fn games() -> PathBuf {
    root().join("games")
}

/// nix-portable, its store, and the nix environments the ports run in.
pub fn toolchain() -> PathBuf {
    root().join("toolchain")
}

/// `BB_DATA_DIR`: `bbport.ini`, the launch settings, mods, patches and the
/// files `run.sh` generates. Shared by every profile.
pub fn bbport_data() -> PathBuf {
    root().join("bbport")
}

/// AnyPS5 conversions, one directory per title, shared by every profile.
pub fn anyps5() -> PathBuf {
    root().join("anyps5")
}

/// Local diffs saved before a port checkout is reset to upstream.
pub fn backups() -> PathBuf {
    root().join("backups")
}

/// FSR 4.1.1 assets built from an AMD DLL, passed as `BB_FSR411_DIR`.
pub fn fsr411() -> PathBuf {
    root().join("fsr4_411")
}

/// A profile's own PS data: saves and failures. Created on first use, and
/// left on disk when the profile is deleted, since it holds saves.
pub fn profile(profile_id: &str) -> PathBuf {
    root().join("profiles").join(profile_id)
}
