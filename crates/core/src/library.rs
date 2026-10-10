//! The game library on disk, laid out the way Steam lays out `steamapps`.
//!
//! ```text
//! <root>/
//!   libraryfolders.vdf                     every root decanter knows about
//!   common/<InstallDir>/                   game files, shared between accounts
//!   manifests/<account_id>/
//!     appmanifest_<storefront>_<id>.acf    who owns it and what state it's in
//!   compatdata/<account_id>/<id>/          that game's Proton prefix
//! ```
//!
//! Ownership here is the storefront account (see [`crate::accounts`]), not
//! the profile it's grouped under — two accounts in the same profile still
//! get their own manifest and their own Proton prefix, the same as two
//! accounts in different profiles would.
//!
//! **The manifests are the source of truth.** Nothing here is mirrored
//! into SQLite: copying a `common/` directory and its manifest onto
//! another machine makes the game appear there, and deleting a manifest by
//! hand uninstalls it. A cache would only be something to fall out of sync
//! with what a user can see and edit.
//!
//! Files are shared across accounts; ownership is not. Two accounts that
//! own the same game get one copy under `common/` and a manifest each — so
//! uninstalling for one account must check the others before deleting
//! anything ([`Library::install_dir_is_shared`]).
//!
//! `compatdata` is per account *and* per game, so a game that wrecks its
//! prefix wrecks only its own, and one account's saves never land in
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
/// as expected to anything else that knows the format; decanter itself has
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
    /// Art picked for a game with no catalog entry behind it. A storefront
    /// game leaves this empty: its catalog already names its art.
    pub art: GameArt,
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
    /// `decanter_gog::save_location`), but not every build reports one, so
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
    pub account_id: String,
    /// Proton build for this game specifically, resolved by umu. `None`
    /// uses whatever the app's default is.
    pub proton: Option<String>,
    pub size_on_disk: Option<u64>,
    pub storefront: Storefront,
    /// umu's `GAMEID` for a game with no storefront id of its own, picked
    /// from the UMU database so protonfixes applies that game's fixes.
    /// `None` runs it as umu's generic `umu-default`.
    pub umu_id: Option<String>,
    /// The database row's store, which is where protonfixes looks for the
    /// fix itself. `None` alongside `umu_id` means umu's own store.
    pub umu_store: Option<String>,
}

/// Image URLs for a game added from outside any storefront, kept in its
/// manifest so copying the manifest elsewhere keeps the art too.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GameArt {
    /// Portrait capsule, what the shelves and the sidebar's cover use.
    pub cover: Option<String>,
    /// Wide banner behind the detail page's title.
    pub hero: Option<String>,
    pub icon: Option<String>,
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

/// Progress through [`Library::move_to`]'s copy fallback.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MoveProgress {
    pub bytes_copied: u64,
    pub total_bytes: u64,
}

impl InstalledGame {
    /// `None` for a file that isn't a manifest decanter can use. Anything
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
        let account_id = match root.get("accountid") {
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
            account_id: account_id.to_owned(),
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
            umu_id: root.get("UmuId").map(str::to_owned),
            umu_store: root.get("UmuStore").map(str::to_owned),
            art: root
                .get_block("Art")
                .map(|art| GameArt {
                    cover: art.get("cover").map(str::to_owned),
                    hero: art.get("hero").map(str::to_owned),
                    icon: art.get("icon").map(str::to_owned),
                })
                .unwrap_or_default(),
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
        root.insert("accountid", self.account_id.clone());
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
        if let Some(id) = self.umu_id.as_ref() {
            root.insert("UmuId", id.clone());
        }
        if let Some(store) = self.umu_store.as_ref() {
            root.insert("UmuStore", store.clone());
        }
        if self.art != GameArt::default() {
            let mut art = Value::block();
            for (key, url) in [
                ("cover", &self.art.cover),
                ("hero", &self.art.hero),
                ("icon", &self.art.icon),
            ] {
                if let Some(url) = url {
                    art.insert(key, url.clone());
                }
            }
            root.insert("Art", art);
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

    /// The Proton prefix for one account's copy of one game. Not created
    /// here — umu creates it on first launch.
    #[must_use]
    #[inline]
    pub fn compatdata(&self, account_id: &str, game_id: &str) -> Option<PathBuf> {
        let account = match safe_component(account_id) {
            Some(value) => value,
            None => return None,
        };
        let game = match safe_component(game_id) {
            Some(value) => value,
            None => return None,
        };
        Some(self.root.join("compatdata").join(account).join(game))
    }

    /// Where the library lives unless someone points decanter elsewhere.
    #[must_use]
    #[inline]
    pub fn default_path() -> Option<PathBuf> {
        Some(crate::data_dir().join("steamapps"))
    }

    /// The actual root under `picked` — a folder a picker handed back —
    /// content goes into.
    ///
    /// Confirmed against a real Steam client: adding a drive as a library
    /// folder from Steam's own Storage settings creates exactly a
    /// `steamapps/` subfolder plus a `libraryfolder.vdf` marker at the
    /// drive root, never files loose at the root itself. `picked` is
    /// treated the same way here — `<picked>/steamapps` if that's not
    /// already there, or `picked` itself if its own name is already
    /// `steamapps` (picking straight into an existing one, decanter's own
    /// or Steam's, rather than nesting `steamapps/steamapps`).
    #[must_use]
    pub fn root_under(picked: &Path) -> PathBuf {
        if picked.file_name().is_some_and(|name| name == "steamapps") {
            picked.to_path_buf()
        } else {
            picked.join("steamapps")
        }
    }

    /// Moves this library's entire root — `common/`, `compatdata/`,
    /// `manifests/`, `libraryfolders.vdf` — to `new_root`, returning the
    /// `Library` rooted there.
    ///
    /// Tries a plain rename first (instant, and the only thing that runs
    /// when `new_root` is on the same filesystem as the current root);
    /// falls back to a recursive copy, verified against a byte count taken
    /// before copying starts, when the rename fails — moving to a
    /// different disk is the actual point of this. The original root is
    /// only removed once every byte the pre-copy walk counted has landed
    /// on the other side, so a failure partway through the copy leaves it
    /// untouched rather than straddling two locations. `on_progress` fires
    /// only during that fallback — the rename path returns before it would
    /// have anything to report.
    ///
    /// # Errors
    /// Fails if `new_root` already has anything in it (refused rather than
    /// risking mixing this library in with whatever's already there), or
    /// any filesystem operation along the way fails.
    #[inline]
    pub fn move_to(&self, new_root: &Path, on_progress: impl FnMut(MoveProgress)) -> io::Result<Self> {
        if new_root.is_dir() && fs::read_dir(new_root)?.next().is_some() {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                format!("{} is not empty", new_root.display()),
            ));
        }

        move_dir(&self.root, new_root, on_progress)?;
        Ok(Self {
            root: new_root.to_path_buf(),
        })
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

    /// Whether anyone other than `(account_id, game_id)` still has files
    /// in `install_dir` — i.e. whether deleting it would take a game out
    /// from under another account.
    #[must_use]
    #[inline]
    pub fn install_dir_is_shared(&self, install_dir: &str, account_id: &str, game_id: &str) -> bool {
        self.scan_all().iter().any(|other| {
            other.install_dir == install_dir && !(other.account_id == account_id && other.game_id == game_id)
        })
    }

    /// Directory holding one account's manifests.
    fn manifest_dir(&self, account_id: &str) -> Option<PathBuf> {
        let account = match safe_component(account_id) {
            Some(value) => value,
            None => return None,
        };
        Some(self.root.join("manifests").join(account))
    }

    #[must_use]
    #[inline]
    pub fn manifest_path(&self, account_id: &str, storefront: Storefront, game_id: &str) -> Option<PathBuf> {
        let dir = match self.manifest_dir(account_id) {
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
    pub fn read_manifest(&self, account_id: &str, storefront: Storefront, game_id: &str) -> Option<InstalledGame> {
        let path = match self.manifest_path(account_id, storefront, game_id) {
            Some(value) => value,
            None => return None,
        };
        read_manifest_file(&path)
    }

    /// Forgets an install. The files under `common/` are left alone —
    /// whether they can go depends on the other accounts, which is
    /// [`Library::install_dir_is_shared`]'s question.
    ///
    /// # Errors
    ///
    /// Fails if the manifest exists on disk but can't be removed.
    #[inline]
    pub fn remove_manifest(
        &self,
        account_id: &str,
        storefront: Storefront,
        game_id: &str,
    ) -> io::Result<Option<InstalledGame>> {
        let Some(path) = self.manifest_path(account_id, storefront, game_id) else {
            tracing::warn!(
                account_id,
                ?storefront,
                game_id,
                "account or game id unusable as a manifest path"
            );
            return Ok(None);
        };
        let Some(game) = read_manifest_file(&path) else {
            // Nothing on record: removing it is what the caller wanted.
            if let Err(err) = fs::remove_file(&path) {
                tracing::debug!(path = %path.display(), error = %err, "stray manifest already gone");
            }
            return Ok(None);
        };
        match fs::remove_file(&path) {
            Ok(()) => {
                tracing::info!(account_id, ?storefront, game_id, "removed install manifest");
            }
            Err(err) => {
                tracing::error!(path = %path.display(), error = %err, "failed to remove manifest file");
                return Err(err);
            }
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
        let path = self.root.join("libraryfolders.vdf");
        let content = match fs::read_to_string(&path) {
            Ok(content) => Some(content),
            Err(err) if err.kind() == io::ErrorKind::NotFound => {
                // trace, not debug, since every fresh install starts
                // without this file and roots() runs on every scan
                tracing::trace!(path = %path.display(), "no libraryfolders.vdf yet");
                None
            }
            Err(err) => {
                tracing::warn!(path = %path.display(), error = %err, "failed to read libraryfolders.vdf");
                None
            }
        };
        let listed = content
            .and_then(|content| match vdf::from_str(&content) {
                Ok(parsed) => Some(parsed),
                Err(err) => {
                    tracing::warn!(path = %path.display(), error = %err, "failed to parse libraryfolders.vdf");
                    None
                }
            })
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

        let roots = if listed.contains(&self.root) {
            listed
        } else {
            iter::once(self.root.clone()).chain(listed).collect()
        };
        tracing::debug!(root_count = roots.len(), "resolved library roots");
        roots
    }

    /// Everything one account owns, by game id.
    #[must_use]
    #[inline]
    pub fn scan(&self, account_id: &str) -> BTreeMap<String, InstalledGame> {
        let Some(directory) = self.manifest_dir(account_id) else {
            tracing::warn!(account_id, "account id unusable as a manifest directory");
            return BTreeMap::new();
        };
        let games: BTreeMap<_, _> = read_manifest_dir(&directory)
            .into_iter()
            .map(|game| (game.game_id.clone(), game))
            .collect();
        tracing::debug!(account_id, game_count = games.len(), "scanned account manifests");
        games
    }

    /// Every manifest in the library, across every account. What
    /// uninstalling consults before deleting shared files.
    #[must_use]
    #[inline]
    pub fn scan_all(&self) -> Vec<InstalledGame> {
        let dir = self.root.join("manifests");
        let entries = match fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(err) if err.kind() == io::ErrorKind::NotFound => {
                tracing::trace!(path = %dir.display(), "no manifests directory yet");
                return Vec::new();
            }
            Err(err) => {
                tracing::warn!(path = %dir.display(), error = %err, "failed to read manifests directory");
                return Vec::new();
            }
        };
        let games: Vec<_> = entries
            .flatten()
            .flat_map(|entry| read_manifest_dir(&entry.path()))
            .collect();
        tracing::debug!(game_count = games.len(), "scanned all accounts' manifests");
        games
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
            Err(err) => {
                tracing::error!(path = %self.root.display(), error = %err, "failed to create library root directory");
                return Err(err);
            }
        }
        let mut entry = Value::block();
        entry.insert("path", self.root.to_string_lossy().into_owned());
        entry.insert("label", "");
        let mut root = Value::block();
        root.insert("0", entry);
        let path = self.root.join("libraryfolders.vdf");
        match fs::write(&path, vdf::to_string("libraryfolders", &root)) {
            Ok(()) => {
                tracing::debug!(path = %path.display(), "wrote libraryfolders.vdf");
                Ok(())
            }
            Err(err) => {
                tracing::error!(path = %path.display(), error = %err, "failed to write libraryfolders.vdf");
                Err(err)
            }
        }
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
        let path = match self.manifest_path(&game.account_id, game.storefront, &game.game_id) {
            Some(value) => value,
            None => {
                tracing::warn!(
                    account_id = %game.account_id,
                    game_id = %game.game_id,
                    "unusable account or game id for manifest path"
                );
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("unusable account or game id for {}", game.game_id),
                ));
            }
        };
        if let Some(parent) = path.parent() {
            match fs::create_dir_all(parent) {
                Ok(()) => {}
                Err(err) => {
                    tracing::error!(path = %parent.display(), error = %err, "failed to create manifest directory");
                    return Err(err);
                }
            }
        }
        match fs::write(&path, vdf::to_string("AppState", &game.to_vdf())) {
            Ok(()) => {
                // A milestone worth seeing at default level: an install
                // just got recorded, unlike the routine per-scan reads above.
                tracing::info!(
                    account_id = %game.account_id,
                    storefront = ?game.storefront,
                    game_id = %game.game_id,
                    "wrote install manifest"
                );
            }
            Err(err) => {
                tracing::error!(path = %path.display(), error = %err, "failed to write manifest file");
                return Err(err);
            }
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
    let content = match fs::read_to_string(path) {
        Ok(content) => content,
        Err(err) if err.kind() == io::ErrorKind::NotFound => {
            // Checking whether one particular game is installed is routine
            // and happens for games that simply aren't — high frequency,
            // low value, unlike the warn below for a real read failure.
            tracing::trace!(path = %path.display(), "no manifest file to read");
            return None;
        }
        Err(err) => {
            tracing::warn!(path = %path.display(), error = %err, "failed to read manifest file");
            return None;
        }
    };
    match vdf::from_str(&content) {
        Ok((_, root)) => {
            let game = InstalledGame::from_vdf(&root);
            if game.is_none() {
                tracing::warn!(path = %path.display(), "manifest missing a required field, skipping");
            }
            game
        }
        Err(err) => {
            tracing::warn!(path = %path.display(), error = %err, "skipping unreadable manifest");
            None
        }
    }
}

/// Every valid manifest directly inside `directory`; anything that isn't
/// shaped like an `appmanifest_*.acf` file is skipped rather than tried.
fn read_manifest_dir(directory: &Path) -> Vec<InstalledGame> {
    let entries = match fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(err) if err.kind() == io::ErrorKind::NotFound => {
            tracing::trace!(path = %directory.display(), "no manifest directory yet");
            return Vec::new();
        }
        Err(err) => {
            tracing::warn!(path = %directory.display(), error = %err, "failed to read manifest directory");
            return Vec::new();
        }
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

/// Moves `src` to `dst`: a rename when both are on one filesystem, else a
/// copy that keeps symlinks as links, with `src` removed only once every
/// byte has landed — a failure partway leaves `src` whole. `on_progress`
/// fires only during the copy.
///
/// # Errors
/// Any filesystem operation along the way failing.
#[inline]
pub fn move_dir(src: &Path, dst: &Path, mut on_progress: impl FnMut(MoveProgress)) -> io::Result<()> {
    if fs::rename(src, dst).is_ok() {
        tracing::info!(from = %src.display(), to = %dst.display(), "moved directory (rename)");
        return Ok(());
    }
    tracing::debug!(
        from = %src.display(),
        to = %dst.display(),
        "rename failed (likely a different filesystem), falling back to copy"
    );
    let total_bytes = directory_size(src)?;
    let mut bytes_copied = 0u64;
    copy_dir_recursive(src, dst, &mut bytes_copied, total_bytes, &mut on_progress)?;
    fs::remove_dir_all(src)?;
    tracing::info!(from = %src.display(), to = %dst.display(), total_bytes, "moved directory (copy)");
    Ok(())
}

/// Copies `src` to `dst`, keeping symlinks as links, and leaves `src` as
/// it was. A failure partway leaves a partial `dst` for the caller to
/// clear.
///
/// # Errors
/// Any filesystem operation along the way failing.
#[inline]
pub fn copy_dir(src: &Path, dst: &Path, mut on_progress: impl FnMut(MoveProgress)) -> io::Result<()> {
    let total_bytes = directory_size(src)?;
    let mut bytes_copied = 0u64;
    copy_dir_recursive(src, dst, &mut bytes_copied, total_bytes, &mut on_progress)?;
    tracing::info!(from = %src.display(), to = %dst.display(), total_bytes, "copied directory");
    Ok(())
}

/// Total bytes of every regular file under `dir`, recursively — the
/// denominator [`move_dir`]'s copy fallback reports progress
/// against, walked once before any copying starts so a mid-copy failure
/// doesn't leave the total itself in question.
///
/// # Errors
/// Reading any directory or entry under `dir` failing.
#[inline]
pub fn directory_size(dir: &Path) -> io::Result<u64> {
    let mut total = 0u64;
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            total += directory_size(&entry.path())?;
        } else {
            total += entry.metadata()?.len();
        }
    }
    Ok(total)
}

/// Copies every file under `src` into `dst` (creating directories as
/// needed), reporting cumulative progress against `total_bytes` after
/// each file. Leaves `src` alone either way — [`move_dir`] removes
/// it itself, only once this returns `Ok`.
fn copy_dir_recursive(
    src: &Path,
    dst: &Path,
    bytes_copied: &mut u64,
    total_bytes: u64,
    on_progress: &mut impl FnMut(MoveProgress),
) -> io::Result<()> {
    fs::create_dir_all(dst)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let dst_path = dst.join(entry.file_name());
        let kind = entry.file_type()?;
        if kind.is_symlink() {
            // Recreated as a link: following it copied what it points at —
            // a Wine prefix's `dosdevices/z:` is `/` — or failed outright
            // on a link to a directory, halfway through the move.
            std::os::unix::fs::symlink(fs::read_link(entry.path())?, &dst_path)?;
        } else if kind.is_dir() {
            copy_dir_recursive(&entry.path(), &dst_path, bytes_copied, total_bytes, on_progress)?;
        } else {
            fs::copy(entry.path(), &dst_path)?;
            *bytes_copied += entry.metadata()?.len();
            on_progress(MoveProgress {
                bytes_copied: *bytes_copied,
                total_bytes,
            });
        }
    }
    Ok(())
}

/// A single path component that can't climb, hide, or reach an absolute
/// path. Every value that reaches this came from a storefront's manifest
/// or from an account id.
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
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("decanter-library-test-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("create temp dir");
        dir
    }

    #[test]
    fn external_game_art_survives_a_manifest_round_trip() {
        let library = Library::new(temp_dir("external-art"));
        let game = InstalledGame {
            art: GameArt {
                cover: Some("https://cdn2.steamgriddb.com/grid/a.png".to_owned()),
                hero: None,
                icon: Some("https://cdn2.steamgriddb.com/icon/b.png".to_owned()),
            },
            branch: None,
            build_id: String::new(),
            cloud_save_last_sync: None,
            cloud_save_path: None,
            dlcs: BTreeMap::new(),
            game_id: "0b8e7c52-5f0e-4a43-a7a4-0d6c0a4f5a10".to_owned(),
            install_dir: "0b8e7c52-5f0e-4a43-a7a4-0d6c0a4f5a10".to_owned(),
            last_played: None,
            launch_executable: Some("/games/Some Game/game.exe".to_owned()),
            name: "Some Game".to_owned(),
            account_id: "profile".to_owned(),
            proton: None,
            size_on_disk: None,
            storefront: Storefront::Unspecified,
            umu_id: Some("umu-endfield".to_owned()),
            umu_store: None,
        };
        library.write_manifest(&game).expect("manifest written");
        let read = library.read_manifest("profile", Storefront::Unspecified, &game.game_id);
        assert_eq!(read, Some(game));
    }

    #[test]
    fn move_to_relocates_every_file_and_removes_the_original() {
        let base = temp_dir("move-to");
        let old_root = base.join("old");
        let new_root = base.join("new");
        fs::create_dir_all(old_root.join("common/SomeGame")).expect("nested dir");
        fs::write(old_root.join("common/SomeGame/game.exe"), b"pretend exe").expect("write file");
        fs::write(old_root.join("libraryfolders.vdf"), b"placeholder").expect("write file");

        let library = Library::new(old_root.clone());
        let moved = library.move_to(&new_root, |_| {}).expect("move succeeds");

        assert_eq!(moved.root(), new_root.as_path());
        assert!(!old_root.exists(), "original root must be gone after a successful move");
        assert_eq!(
            fs::read(new_root.join("common/SomeGame/game.exe")).expect("moved file readable"),
            b"pretend exe"
        );
        assert_eq!(
            fs::read(new_root.join("libraryfolders.vdf")).expect("moved file readable"),
            b"placeholder"
        );

        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn move_to_refuses_a_non_empty_destination() {
        let base = temp_dir("move-to-refuse");
        let old_root = base.join("old");
        let new_root = base.join("new");
        fs::create_dir_all(&old_root).expect("create dir");
        fs::write(old_root.join("marker"), b"x").expect("write file");
        fs::create_dir_all(&new_root).expect("create dir");
        fs::write(new_root.join("already-here"), b"unrelated").expect("write file");

        let library = Library::new(old_root.clone());
        let result = library.move_to(&new_root, |_| {});

        assert!(result.is_err(), "a non-empty destination must be refused");
        assert!(
            old_root.join("marker").is_file(),
            "original must be untouched when refused"
        );

        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn copy_dir_recursive_mirrors_every_file_and_reports_final_progress() {
        let base = temp_dir("copy-walk");
        let src = base.join("src");
        let dst = base.join("dst");
        fs::create_dir_all(src.join("nested")).expect("nested dir");
        fs::write(src.join("a.txt"), b"aaaa").expect("write file");
        fs::write(src.join("nested/b.txt"), b"bb").expect("write file");

        let total = directory_size(&src).expect("sum sizes");
        assert_eq!(total, 6);

        let mut bytes_copied = 0u64;
        let mut last_progress = None;
        copy_dir_recursive(&src, &dst, &mut bytes_copied, total, &mut |progress| {
            last_progress = Some(progress);
        })
        .expect("copy succeeds");

        assert_eq!(
            last_progress,
            Some(MoveProgress {
                bytes_copied: 6,
                total_bytes: 6
            })
        );
        assert_eq!(fs::read(dst.join("a.txt")).expect("copied file readable"), b"aaaa");
        assert_eq!(fs::read(dst.join("nested/b.txt")).expect("copied file readable"), b"bb");
        assert!(src.exists(), "copy_dir_recursive alone must never touch the source");

        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn copy_dir_recursive_keeps_links_as_links() {
        let base = temp_dir("copy-links");
        let (src, dst, elsewhere) = (base.join("src"), base.join("dst"), base.join("elsewhere"));
        fs::create_dir_all(src.join("dosdevices")).expect("src dirs");
        fs::create_dir_all(&elsewhere).expect("target dir");
        fs::write(elsewhere.join("big.bin"), b"not to be copied").expect("target file");
        std::os::unix::fs::symlink(&elsewhere, src.join("dosdevices/z:")).expect("dir link");
        std::os::unix::fs::symlink(elsewhere.join("big.bin"), src.join("file-link")).expect("file link");

        let mut bytes_copied = 0u64;
        copy_dir_recursive(&src, &dst, &mut bytes_copied, 0, &mut |_| {}).expect("copy succeeds");

        assert_eq!(
            fs::read_link(dst.join("dosdevices/z:")).expect("still a link"),
            elsewhere
        );
        assert_eq!(
            fs::read_link(dst.join("file-link")).expect("still a link"),
            elsewhere.join("big.bin")
        );
        assert_eq!(bytes_copied, 0, "nothing behind a link is copied");

        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn root_under_adds_a_steamapps_level_under_a_plain_folder() {
        let picked = PathBuf::from("/home/deck/data/Decanter");
        assert_eq!(Library::root_under(&picked), picked.join("steamapps"));
    }

    #[test]
    fn root_under_uses_an_existing_steamapps_folder_directly() {
        let picked = PathBuf::from("/home/deck/data/Decanter/steamapps");
        assert_eq!(Library::root_under(&picked), picked);
    }
}
