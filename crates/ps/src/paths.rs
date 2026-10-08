//! Where decanter keeps what it generates for PS games: one directory of
//! its own beside the PC library, never inside a `steamapps` root, so
//! `decanter_core::Library` scans, moves and the Steam export never see it.

use std::path::PathBuf;

pub fn root() -> PathBuf {
    decanter_core::data_dir().join("psport")
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
