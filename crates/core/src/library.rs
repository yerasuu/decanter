//! The game library on disk, laid out the way Steam lays out `steamapps`.
//!
//! ```text
//! <root>/
//!   libraryfolders.vdf                     every root achv knows about
//!   common/<InstallDir>/                   game files, shared between profiles
//!   manifests/<profile_id>/
//!     appmanifest_<storefront>_<id>.acf    who owns it and what state it's in
//!   compatdata/<profile_id>/<id>/          that game's Proton prefix
//! ```
//!
//! **The manifests are the source of truth.** Nothing here is mirrored
//! into SQLite: copying a `common/` directory and its manifest onto
//! another machine makes the game appear there, and deleting a manifest by
//! hand uninstalls it. A cache would only be something to fall out of sync
//! with what a user can see and edit.
//!
//! Files are shared across profiles; ownership is not. Two profiles that
//! own the same game get one copy under `common/` and a manifest each — so
//! uninstalling for one profile must check the others before deleting
//! anything ([`Library::install_dir_is_shared`]).
//!
//! `compatdata` is per profile *and* per game, so a game that wrecks its
//! prefix wrecks only its own, and one profile's saves never land in
//! another's. It is created lazily, on first launch rather than at install
//! time: a fresh Proton prefix is ~660 MB, and a game that is installed
//! but never played should cost nothing.

// Both lints want a narrower crate than this one has: this is a std app, not a
// `no_std`/`alloc`-only library, so migrating `BTreeMap` and `iter::once` off `std`
// buys nothing while the rest of the file stays on `std::fs`/`std::io`. Item-level
// `#[allow]` reads as "useless attribute" to clippy for these two (it wants the
// inner form), so this covers the whole file instead.
#![expect(
    clippy::std_instead_of_alloc,
    reason = "std app; alloc has no fs/io to migrate the rest of this file to"
)]
#![expect(
    clippy::std_instead_of_core,
    reason = "std app; core has no fs/io to migrate the rest of this file to"
)]
// Only fires on some compilation targets (lib vs tests), so `expect` can't
// be used here without an "unfulfilled expectation" warning on the other.
#![allow(
    clippy::self_named_module_files,
    reason = "convention here is `<name>.rs` plus a sibling `<name>/tests.rs`, see CLAUDE.md"
)]
#![expect(
    clippy::implicit_return,
    reason = "directly contradicts needless_return from clippy::all, also enabled"
)]
#![expect(
    clippy::large_stack_frames,
    reason = "several structs/derives below cost more than the 512-byte threshold by design"
)]

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::iter;
use std::path::{Path, PathBuf};

use crate::catalog::Storefront;
use crate::vdf::{self, Value};

/// Steam's `StateFlags` for "fully installed". Written so the files read
/// as expected to anything else that knows the format; achv itself has
/// no partially-installed state — an install either finished or its
/// staging directory was thrown away.
const STATE_FULLY_INSTALLED: u64 = 4;

/// Every storefront, checked in turn against a manifest's raw `storefront`
/// string to resolve it back to a [`Storefront`] variant.
const STOREFRONTS: [Storefront; 8] = [
    Storefront::Unspecified,
    Storefront::Steam,
    Storefront::EpicGames,
    Storefront::Gog,
    Storefront::AmazonGames,
    Storefront::EaApp,
    Storefront::UbisoftConnect,
    Storefront::BattleNet,
];

/// One installed game, as its manifest records it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstalledGame {
    /// The storefront branch this build came from — a beta opt-in, most
    /// often. `None` is mainline, and also what an install from before
    /// branches existed reads as, which is the same thing.
    pub branch: Option<String>,
    /// The storefront's build id, so a later fetch can tell whether what's
    /// on disk is current.
    pub build_id: String,
    /// Unix seconds of the last successful cloud saves sync, used as the
    /// "changed since when" threshold for the next one. `None` before the
    /// first sync.
    pub cloud_save_last_sync: Option<i64>,
    /// Local folder synced against this game's Cloud Saves bucket. `None`
    /// until it's set — GOG's remote-config can often name it (see
    /// `achv_gog::save_location`), but not every build reports one, so
    /// this still falls back to the user picking a folder by hand.
    pub cloud_save_path: Option<String>,
    /// The DLC installed into this game's directory, by product id.
    ///
    /// DLC are owned as separate products but are not separate installs:
    /// their files land in the base game's directory, mixed in with it.
    /// Recorded here for the same reason Steam records `InstalledDepots` on
    /// the app rather than as apps of their own — there is one directory,
    /// so there is one manifest.
    pub dlcs: BTreeMap<String, InstalledDlc>,
    /// The storefront's own product id.
    pub game_id: String,
    /// Directory name under `common/`, from the storefront's own manifest
    /// — a single path component, never a nested path.
    pub install_dir: String,
    /// Unix seconds, or `None` if it never has been.
    pub last_played: Option<i64>,
    /// The executable to launch, relative to the install directory and
    /// written with forward slashes. `None` when none could be identified
    /// — the game is installed, there's just nothing to start.
    pub launch_executable: Option<String>,
    pub name: String,
    pub profile_id: String,
    /// Proton build for this game specifically, resolved by umu. `None`
    /// uses whatever the app's default is.
    pub proton: Option<String>,
    pub size_on_disk: Option<u64>,
    pub storefront: Storefront,
}

/// One DLC installed into a game.
///
/// No file list: the files are indistinguishable from the base game's once
/// they land, which is also why removing a single DLC isn't offered —
/// see [`Library::remove_manifest`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstalledDlc {
    /// The storefront's build id for the DLC itself, so a later fetch can
    /// tell whether what's on disk is current.
    pub build_id: String,
    pub title: String,
}

/// A `steamapps`-shaped directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Library {
    /// Filesystem root of this library.
    root: PathBuf,
}

impl InstalledGame {
    /// `None` for a file that isn't a manifest achv can use. Anything
    /// missing a game id, storefront or install directory is unusable
    /// rather than partially usable.
    fn from_vdf(root: &Value) -> Option<Self> {
        let storefront = match STOREFRONTS
            .iter()
            .find(|candidate| {
                candidate
                    .as_str_name()
                    .eq_ignore_ascii_case(root.get("storefront").unwrap_or_default())
            })
            .copied()
        {
            Some(value) => value,
            None => return None,
        };
        let profile_id = match root.get("profileid") {
            Some(value) => value,
            None => return None,
        };
        let game_id = match root.get("gameid") {
            Some(value) => value,
            None => return None,
        };
        let install_dir = match root.get("installdir") {
            Some(value) => value,
            None => return None,
        };
        Some(Self {
            profile_id: profile_id.to_owned(),
            storefront,
            game_id: game_id.to_owned(),
            name: root.get("name").unwrap_or_default().to_owned(),
            install_dir: install_dir.to_owned(),
            build_id: root.get("buildid").unwrap_or_default().to_owned(),
            size_on_disk: root.get_u64("SizeOnDisk"),
            launch_executable: root.get("LaunchExecutable").map(str::to_owned),
            last_played: root.get("LastPlayed").and_then(|value| value.parse().ok()),
            proton: root.get("Proton").map(str::to_owned),
            branch: root.get("Branch").map(str::to_owned),
            dlcs: root
                .get_block("InstalledDLC")
                .and_then(Value::as_block)
                .map(|block| {
                    block
                        .iter()
                        .filter_map(|(id, entry)| {
                            let title = match entry.get("title") {
                                Some(value) => value,
                                None => return None,
                            };
                            Some((
                                id.clone(),
                                InstalledDlc {
                                    title: title.to_owned(),
                                    build_id: entry.get("buildid").unwrap_or_default().to_owned(),
                                },
                            ))
                        })
                        .collect()
                })
                .unwrap_or_default(),
            cloud_save_path: root.get("CloudSavePath").map(str::to_owned),
            cloud_save_last_sync: root.get("CloudSaveLastSync").and_then(|value| value.parse().ok()),
        })
    }

    /// The on-disk manifest file name for one game, or `None` if the id
    /// can't be used in a path.
    fn manifest_file_name(storefront: Storefront, game_id: &str) -> Option<String> {
        // The id lands in a filename, so the same rule as an install root
        // applies: ids are digits or a short slug, anything else is a
        // malformed response rather than a name needing quoting.
        if game_id.is_empty()
            || !game_id
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '-')
        {
            return None;
        }
        Some(format!(
            "appmanifest_{}_{game_id}.acf",
            storefront.as_str_name().to_ascii_lowercase()
        ))
    }

    /// Serializes this game into the shape Steam's own `.acf` files use.
    fn to_vdf(&self) -> Value {
        let mut root = Value::block();
        root.insert("gameid", self.game_id.clone());
        root.insert("storefront", self.storefront.as_str_name());
        root.insert("profileid", self.profile_id.clone());
        root.insert("name", self.name.clone());
        root.insert("installdir", self.install_dir.clone());
        root.insert("buildid", self.build_id.clone());
        root.insert("StateFlags", STATE_FULLY_INSTALLED);
        if let Some(size) = self.size_on_disk {
            root.insert("SizeOnDisk", size);
        }
        if let Some(executable) = self.launch_executable.as_ref() {
            root.insert("LaunchExecutable", executable.clone());
        }
        // Absent rather than `"0"`: Steam writes 0 for never-played, but
        // "never" and "at the epoch" are worth telling apart here.
        if let Some(last_played) = self.last_played {
            root.insert("LastPlayed", last_played);
        }
        if let Some(proton) = self.proton.as_ref() {
            root.insert("Proton", proton.clone());
        }
        if let Some(branch) = self.branch.as_ref() {
            root.insert("Branch", branch.clone());
        }
        if !self.dlcs.is_empty() {
            let mut dlcs = Value::block();
            for (id, dlc) in &self.dlcs {
                let mut entry = Value::block();
                entry.insert("title", dlc.title.clone());
                entry.insert("buildid", dlc.build_id.clone());
                dlcs.insert(id.clone(), entry);
            }
            root.insert("InstalledDLC", dlcs);
        }
        if let Some(path) = self.cloud_save_path.as_ref() {
            root.insert("CloudSavePath", path.clone());
        }
        if let Some(last_sync) = self.cloud_save_last_sync {
            root.insert("CloudSaveLastSync", last_sync);
        }
        root
    }
}

impl Library {
    #[must_use]
    #[inline]
    pub fn common(&self) -> PathBuf {
        self.root.join("common")
    }

    /// The Proton prefix for one profile's copy of one game. Not created
    /// here — umu creates it on first launch.
    #[must_use]
    #[inline]
    pub fn compatdata(&self, profile_id: &str, game_id: &str) -> Option<PathBuf> {
        let profile = match safe_component(profile_id) {
            Some(value) => value,
            None => return None,
        };
        let game = match safe_component(game_id) {
            Some(value) => value,
            None => return None,
        };
        Some(self.root.join("compatdata").join(profile).join(game))
    }

    /// Where the library lives unless someone points achv elsewhere.
    #[must_use]
    #[inline]
    pub fn default_path() -> Option<PathBuf> {
        Some(crate::data_dir().join("steamapps"))
    }

    /// Where a game's files go. `None` for an install directory that isn't
    /// a single, ordinary path component — these come from a storefront's
    /// manifest, i.e. from the network.
    #[must_use]
    #[inline]
    pub fn install_dir(&self, install_dir: &str) -> Option<PathBuf> {
        let component = match safe_component(install_dir) {
            Some(value) => value,
            None => return None,
        };
        Some(self.common().join(component))
    }

    /// Whether anyone other than `(profile_id, game_id)` still has files
    /// in `install_dir` — i.e. whether deleting it would take a game out
    /// from under another profile.
    #[must_use]
    #[inline]
    pub fn install_dir_is_shared(&self, install_dir: &str, profile_id: &str, game_id: &str) -> bool {
        self.scan_all().iter().any(|other| {
            other.install_dir == install_dir && !(other.profile_id == profile_id && other.game_id == game_id)
        })
    }

    /// Directory holding one profile's manifests.
    fn manifest_dir(&self, profile_id: &str) -> Option<PathBuf> {
        let profile = match safe_component(profile_id) {
            Some(value) => value,
            None => return None,
        };
        Some(self.root.join("manifests").join(profile))
    }

    #[must_use]
    #[inline]
    pub fn manifest_path(&self, profile_id: &str, storefront: Storefront, game_id: &str) -> Option<PathBuf> {
        let dir = match self.manifest_dir(profile_id) {
            Some(value) => value,
            None => return None,
        };
        let file_name = match InstalledGame::manifest_file_name(storefront, game_id) {
            Some(value) => value,
            None => return None,
        };
        Some(dir.join(file_name))
    }

    // clippy::impl_trait_in_params wants a named generic here, but that trips
    // clippy::inline_trait_bounds (nursery) right back since `T` appears only
    // once — kept `impl Trait`, the form both `all` and this file's other
    // constructors already use.
    #[inline]
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    #[must_use]
    #[inline]
    pub fn read_manifest(&self, profile_id: &str, storefront: Storefront, game_id: &str) -> Option<InstalledGame> {
        let path = match self.manifest_path(profile_id, storefront, game_id) {
            Some(value) => value,
            None => return None,
        };
        read_manifest_file(&path)
    }

    /// Forgets an install. The files under `common/` are left alone —
    /// whether they can go depends on the other profiles, which is
    /// [`Library::install_dir_is_shared`]'s question.
    ///
    /// # Errors
    ///
    /// Fails if the manifest exists on disk but can't be removed.
    #[inline]
    pub fn remove_manifest(
        &self,
        profile_id: &str,
        storefront: Storefront,
        game_id: &str,
    ) -> io::Result<Option<InstalledGame>> {
        let Some(path) = self.manifest_path(profile_id, storefront, game_id) else {
            return Ok(None);
        };
        let Some(game) = read_manifest_file(&path) else {
            // Nothing on record: removing it is what the caller wanted.
            if let Err(err) = fs::remove_file(&path) {
                tracing::debug!("stray manifest {} already gone: {err}", path.display());
            }
            return Ok(None);
        };
        match fs::remove_file(&path) {
            Ok(()) => {}
            Err(err) => return Err(err),
        }
        Ok(Some(game))
    }

    #[must_use]
    #[inline]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The roots `libraryfolders.vdf` lists, this one included. A missing
    /// or unreadable file yields just this root, which is the state a
    /// fresh install is in.
    #[must_use]
    #[inline]
    pub fn roots(&self) -> Vec<PathBuf> {
        let listed = fs::read_to_string(self.root.join("libraryfolders.vdf"))
            .ok()
            .and_then(|content| vdf::from_str(&content).ok())
            .and_then(|(_, root)| {
                let block = match root.as_block() {
                    Some(value) => value,
                    None => return None,
                };
                Some(
                    block
                        .values()
                        .filter_map(|entry| entry.get("path"))
                        .map(PathBuf::from)
                        .collect::<Vec<_>>(),
                )
            })
            .unwrap_or_default();

        if listed.contains(&self.root) {
            listed
        } else {
            iter::once(self.root.clone()).chain(listed).collect()
        }
    }

    /// Everything one profile owns, by game id.
    #[must_use]
    #[inline]
    pub fn scan(&self, profile_id: &str) -> BTreeMap<String, InstalledGame> {
        let Some(directory) = self.manifest_dir(profile_id) else {
            return BTreeMap::new();
        };
        read_manifest_dir(&directory)
            .into_iter()
            .map(|game| (game.game_id.clone(), game))
            .collect()
    }

    /// Every manifest in the library, across every profile. What
    /// uninstalling consults before deleting shared files.
    #[must_use]
    #[inline]
    pub fn scan_all(&self) -> Vec<InstalledGame> {
        let Ok(entries) = fs::read_dir(self.root.join("manifests")) else {
            return Vec::new();
        };
        entries
            .flatten()
            .flat_map(|entry| read_manifest_dir(&entry.path()))
            .collect()
    }

    /// Announces this root in Steam's own format.
    ///
    /// One entry today. The file exists from the start anyway so that
    /// adding a second disk later is a new entry rather than a migration.
    ///
    /// # Errors
    ///
    /// Fails if the root directory or the file itself can't be written.
    #[inline]
    pub fn write_libraryfolders(&self) -> io::Result<()> {
        match fs::create_dir_all(&self.root) {
            Ok(()) => {}
            Err(err) => return Err(err),
        }
        let mut entry = Value::block();
        entry.insert("path", self.root.to_string_lossy().into_owned());
        entry.insert("label", "");
        let mut root = Value::block();
        root.insert("0", entry);
        fs::write(
            self.root.join("libraryfolders.vdf"),
            vdf::to_string("libraryfolders", &root),
        )
    }

    /// Records `game` as installed. Also refreshes `libraryfolders.vdf`,
    /// so a library that has anything in it always announces itself.
    ///
    /// # Errors
    ///
    /// Fails if `game`'s ids can't form a path, or the manifest can't be
    /// written to disk.
    #[inline]
    pub fn write_manifest(&self, game: &InstalledGame) -> io::Result<()> {
        let path = match self.manifest_path(&game.profile_id, game.storefront, &game.game_id) {
            Some(value) => value,
            None => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("unusable profile or game id for {}", game.game_id),
                ));
            }
        };
        if let Some(parent) = path.parent() {
            match fs::create_dir_all(parent) {
                Ok(()) => {}
                Err(err) => return Err(err),
            }
        }
        match fs::write(&path, vdf::to_string("AppState", &game.to_vdf())) {
            Ok(()) => {}
            Err(err) => return Err(err),
        }
        self.write_libraryfolders()
    }
}

/// Reads and parses one manifest file; `None` if it's missing, unreadable,
/// or not usable ([`InstalledGame::from_vdf`]).
// `clippy::question_mark_used` wants this rewritten without `?`, but the
// only way to do that without a bare `Err(_)` arm (which this codebase's
// own humanization scanner rejects as an undocumented wildcard match) is
// `.ok()?`, which still uses `?`. Kept as-is.
#[expect(clippy::question_mark_used, reason = "see comment above")]
fn read_manifest_file(path: &Path) -> Option<InstalledGame> {
    let content = fs::read_to_string(path).ok()?;
    match vdf::from_str(&content) {
        Ok((_, root)) => InstalledGame::from_vdf(&root),
        Err(err) => {
            tracing::warn!("skipping unreadable manifest {}: {err}", path.display());
            None
        }
    }
}

/// Every valid manifest directly inside `directory`; anything that isn't
/// shaped like an `appmanifest_*.acf` file is skipped rather than tried.
fn read_manifest_dir(directory: &Path) -> Vec<InstalledGame> {
    let Ok(entries) = fs::read_dir(directory) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter(|entry| {
            entry.file_name().to_str().is_some_and(|name| {
                name.starts_with("appmanifest_")
                    && Path::new(name)
                        .extension()
                        .is_some_and(|ext| ext.eq_ignore_ascii_case("acf"))
            })
        })
        .filter_map(|entry| read_manifest_file(&entry.path()))
        .collect()
}

/// A single path component that can't climb, hide, or reach an absolute
/// path. Every value that reaches this came from a storefront's manifest
/// or from a profile id.
fn safe_component(value: &str) -> Option<&str> {
    if value.is_empty()
        || value == "."
        || value == ".."
        || value.contains('/')
        || value.contains('\\')
        || value.contains('\0')
    {
        return None;
    }
    Some(value)
}

#[cfg(test)]
mod tests;
