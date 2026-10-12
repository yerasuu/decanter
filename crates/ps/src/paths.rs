//! Where decanter keeps everything PS: a `psport` folder of its own inside
//! the active profile's library root, beside `common/` and `manifests/`.
//! It lives on the same disk as the PC games and moves with the library;
//! `decanter_core::Library` only ever reads the folders it knows by name,
//! so its scans and the Steam export never see this one.

use std::path::{Path, PathBuf};
use std::sync::RwLock;

/// Set by [`use_library`] whenever the active profile is known; read when
/// something starts, written once per profile switch.
static ACTIVE: RwLock<Option<Root>> = RwLock::new(None);

/// One library's `psport` folder. Jobs take it when they start, so a
/// profile switch while they run never moves them to another library.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Root(PathBuf);

/// Before a profile is active: the default library's folder, where a
/// profile that never chose its own library keeps it too.
fn default_root() -> Root {
    Root(
        decanter_core::Library::default_path()
            .unwrap_or_else(|| decanter_core::data_dir().join("steamapps"))
            .join("psport"),
    )
}

/// The active profile's folder, for what lists or edits its data now.
pub fn active() -> Root {
    match ACTIVE.read() {
        Ok(root) => root.clone().unwrap_or_else(default_root),
        Err(poisoned) => poisoned.into_inner().clone().unwrap_or_else(default_root),
    }
}

/// Points [`active`] at `library_root`'s `psport` folder.
pub fn use_library(library_root: &Path) {
    let root = Root(library_root.join("psport"));
    match ACTIVE.write() {
        Ok(mut current) => *current = Some(root),
        Err(poisoned) => *poisoned.into_inner() = Some(root),
    }
}

impl Root {
    pub(crate) fn join(&self, path: impl AsRef<Path>) -> PathBuf {
        self.0.join(path)
    }

    /// The library folder this one sits in, where decanter's other data
    /// for the profile goes.
    pub fn library(&self) -> &Path {
        self.0.parent().unwrap_or(&self.0)
    }

    /// The only folder PS games are read from.
    pub fn games(&self) -> PathBuf {
        self.join("games")
    }

    /// nix-portable, its store, and the nix environments the ports run in.
    pub fn toolchain(&self) -> PathBuf {
        self.join("toolchain")
    }

    /// `BB_DATA_DIR`: `bbport.ini`, the launch settings, mods, patches and the
    /// files `run.sh` generates. Shared by every profile.
    pub fn bbport_data(&self) -> PathBuf {
        self.join("bbport")
    }

    /// AnyPS5 conversions, one directory per title, shared by every profile.
    pub fn anyps5(&self) -> PathBuf {
        self.join("anyps5")
    }

    /// Local diffs saved before a port checkout is reset to upstream.
    pub fn backups(&self) -> PathBuf {
        self.join("backups")
    }

    /// FSR 4.1.1 assets built from an AMD DLL, passed as `BB_FSR411_DIR`.
    pub fn fsr411(&self) -> PathBuf {
        self.join("fsr4_411")
    }

    /// A profile's own PS data: saves and failures. Created on first use, and
    /// left on disk when the profile is deleted, since it holds saves.
    pub fn profile(&self, profile_id: &str) -> PathBuf {
        self.join("profiles").join(profile_id)
    }
}
