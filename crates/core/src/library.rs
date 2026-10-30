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
use std::sync::atomic::{AtomicBool, Ordering};

use crate::catalog::Storefront;
use crate::platform;
use crate::vdf::{self, Value};

/// Steam's `StateFlags` for "fully installed". Written so the files read
/// as expected to anything else that knows the format; nothing under
/// `common/` is ever partially installed — an unfinished download stays in
/// its staging folder under `downloading/`, with its journal, until it
/// finishes or is cancelled.
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
    /// The storefront's tag of the one language whose files were installed
    /// (`es-ES`). `None` is every language the build has, which is also what
    /// an install from before languages were chosen reads as.
    pub language: Option<String>,
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
    /// [`crate::install_fingerprint`] of the files `size_on_disk` was
    /// measured from, for a game whose size decanter measures itself rather
    /// than takes from an installer; it is measured again only once this
    /// stops matching.
    pub size_fingerprint: Option<String>,
    pub size_on_disk: Option<u64>,
    pub storefront: Storefront,
    /// umu's `GAMEID` for a game with no storefront id of its own, picked
    /// from the UMU database so protonfixes applies that game's fixes.
    /// `None` runs it as umu's generic `umu-default`.
    pub umu_id: Option<String>,
    /// The database row's store, which is where protonfixes looks for the
    /// fix itself. `None` alongside `umu_id` means umu's own store.
    pub umu_store: Option<String>,
    /// The game whose prefix this one runs in, when it was installed by a
    /// launcher (HoYoPlay, say) that is itself the other game: its login,
    /// anticheat and data live in that prefix, not one of its own.
    pub prefix_of: Option<String>,
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

/// One or more `steamapps`-shaped directories, read as one library — a
/// profile's games can sit on several disks, the way Steam's library
/// folders do.
///
/// Every game lives whole in one root: its manifest, its files under
/// `common/` and its prefix under `compatdata/`. Anything about a game
/// that's already there is looked up in whichever root has it; anything
/// new goes to the primary root, which is also where the rest of a
/// profile's data (`psport/`, `libraryfolders.vdf`) lives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Library {
    /// Filesystem root of the primary library folder.
    root: PathBuf,
    /// The other library folders, in the order they were added.
    extra_roots: Vec<PathBuf>,
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
            size_fingerprint: root.get("SizeFingerprint").map(str::to_owned),
            launch_executable: root.get("LaunchExecutable").map(str::to_owned),
            last_played: root.get("LastPlayed").and_then(|value| value.parse().ok()),
            proton: root.get("Proton").map(str::to_owned),
            branch: root.get("Branch").map(str::to_owned),
            language: root.get("Language").map(str::to_owned),
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
            prefix_of: root.get("PrefixOf").map(str::to_owned),
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
        if let Some(fingerprint) = self.size_fingerprint.as_ref() {
            root.insert("SizeFingerprint", fingerprint.clone());
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
        if let Some(language) = self.language.as_ref() {
            root.insert("Language", language.clone());
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
        if let Some(host) = self.prefix_of.as_ref() {
            root.insert("PrefixOf", host.clone());
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
    /// The primary root's `common/`, where a new install goes.
    #[must_use]
    #[inline]
    pub fn common(&self) -> PathBuf {
        self.root.join("common")
    }

    /// Every root, primary first.
    fn all_roots(&self) -> impl Iterator<Item = &Path> {
        iter::once(self.root.as_path()).chain(self.extra_roots.iter().map(PathBuf::as_path))
    }

    /// The root holding `game_id`'s manifest for `account_id`, under any
    /// storefront — so a game's prefix and files are found beside it.
    fn root_holding(&self, account_id: &str, game_id: &str) -> Option<&Path> {
        self.all_roots().find(|root| {
            STOREFRONTS.iter().any(|storefront| {
                manifest_path_in(root, account_id, *storefront, game_id).is_some_and(|path| path.is_file())
            })
        })
    }

    /// The library folder holding `game_id`'s manifest for `account_id`.
    #[must_use]
    #[inline]
    pub fn root_of(&self, account_id: &str, game_id: &str) -> Option<PathBuf> {
        self.root_holding(account_id, game_id).map(Path::to_path_buf)
    }

    /// The Proton prefix for one account's copy of one game, in the root
    /// that holds the game (or holds a prefix for it already), else the
    /// primary one. Not created here — umu creates it on first launch.
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
        let prefix_in = |root: &Path| root.join("compatdata").join(account).join(game);
        let root = self
            .root_holding(account_id, game_id)
            .or_else(|| self.all_roots().find(|root| prefix_in(root).is_dir()))
            .unwrap_or(&self.root);
        Some(prefix_in(root))
    }

    /// What the first-run screen suggests when no library exists yet: the
    /// old default library's place on Linux, and on Windows under the local
    /// app data rather than the roaming one, so games don't roam with the
    /// user's profile.
    #[must_use]
    #[inline]
    pub fn default_path() -> Option<PathBuf> {
        platform::default_library_dir()
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
        // Only the primary root moves; the other folders stay where they are.
        Ok(Self {
            root: new_root.to_path_buf(),
            extra_roots: self.extra_roots.clone(),
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
        let existing = self
            .all_roots()
            .map(|root| root.join("common").join(component))
            .find(|dir| dir.is_dir());
        Some(existing.unwrap_or_else(|| self.common().join(component)))
    }

    /// Whether anyone other than `(account_id, game_id)` still has files
    /// in `install_dir` — i.e. whether deleting it would take a game out
    /// from under another account. Only a manifest in the same root counts:
    /// a folder of that name on another disk is other files.
    #[must_use]
    #[inline]
    pub fn install_dir_is_shared(&self, install_dir: &str, account_id: &str, game_id: &str) -> bool {
        let Some(files) = self.install_dir(install_dir) else {
            return false;
        };
        self.all_roots()
            .filter(|root| files.starts_with(root))
            .flat_map(scan_root)
            .any(|other| {
                other.install_dir == install_dir && !(other.account_id == account_id && other.game_id == game_id)
            })
    }

    /// Where `game_id`'s manifest is, in whichever root has it, else where
    /// a new one goes: the primary root.
    #[must_use]
    #[inline]
    pub fn manifest_path(&self, account_id: &str, storefront: Storefront, game_id: &str) -> Option<PathBuf> {
        self.all_roots()
            .filter_map(|root| manifest_path_in(root, account_id, storefront, game_id))
            .find(|path| path.is_file())
            .or_else(|| manifest_path_in(&self.root, account_id, storefront, game_id))
    }

    // clippy::impl_trait_in_params wants a named generic here, but that trips
    // clippy::inline_trait_bounds (nursery) right back since `T` appears only
    // once — kept `impl Trait`, the form both `all` and this file's other
    // constructors already use.
    #[inline]
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            extra_roots: Vec::new(),
        }
    }

    /// Adds more library folders to read games from. The primary root, or
    /// one listed twice, is left out.
    #[must_use]
    #[inline]
    pub fn with_extra_roots(mut self, roots: impl IntoIterator<Item = PathBuf>) -> Self {
        for root in roots {
            if root != self.root && !self.extra_roots.contains(&root) {
                self.extra_roots.push(root);
            }
        }
        self
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

    /// The same folders with `root` as the primary one, so what a new
    /// install writes — files, manifest, prefix — lands there. A root that
    /// isn't one of this library's folders changes nothing.
    #[must_use]
    #[inline]
    pub fn targeting(mut self, root: &Path) -> Self {
        if let Some(index) = self.extra_roots.iter().position(|extra| extra == root) {
            let previous = std::mem::replace(&mut self.root, self.extra_roots.remove(index));
            self.extra_roots.insert(0, previous);
        }
        self
    }

    /// Every library folder, primary first.
    #[must_use]
    #[inline]
    pub fn roots(&self) -> Vec<PathBuf> {
        self.all_roots().map(Path::to_path_buf).collect()
    }

    /// Everything one account owns, by game id.
    #[must_use]
    #[inline]
    pub fn scan(&self, account_id: &str) -> BTreeMap<String, InstalledGame> {
        let Some(account) = safe_component(account_id) else {
            tracing::warn!(account_id, "account id unusable as a manifest directory");
            return BTreeMap::new();
        };
        let mut games = BTreeMap::new();
        for root in self.all_roots() {
            for game in read_manifest_dir(&root.join("manifests").join(account)) {
                // Primary first: a game recorded in two roots is read from
                // the first.
                games.entry(game.game_id.clone()).or_insert(game);
            }
        }
        tracing::debug!(account_id, game_count = games.len(), "scanned account manifests");
        games
    }

    /// Every manifest in the library, across every account. What
    /// uninstalling consults before deleting shared files.
    #[must_use]
    #[inline]
    pub fn scan_all(&self) -> Vec<InstalledGame> {
        let games: Vec<_> = self.all_roots().flat_map(scan_root).collect();
        tracing::debug!(game_count = games.len(), "scanned all accounts' manifests");
        games
    }

    /// Writes `game`'s manifest into the folder `to` and drops the copy in
    /// whichever folder had it, once its files and prefix have moved there.
    /// `game` is written as given, so a path that moved with it can be
    /// updated in the same step.
    ///
    /// # Errors
    ///
    /// Fails if `game`'s ids can't form a path, or either file operation
    /// fails; a failure writing leaves the old manifest where it was.
    #[inline]
    pub fn relocate_manifest(&self, game: &InstalledGame, to: &Path) -> io::Result<()> {
        let Some(old) = self.manifest_path(&game.account_id, game.storefront, &game.game_id) else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "unusable account or game id",
            ));
        };
        let Some(new) = manifest_path_in(to, &game.account_id, game.storefront, &game.game_id) else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "unusable account or game id",
            ));
        };
        if let Some(parent) = new.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&new, vdf::to_string("AppState", &game.to_vdf()))?;
        if old != new {
            fs::remove_file(&old)?;
        }
        tracing::info!(game_id = %game.game_id, to = %to.display(), "manifest moved to another library folder");
        Ok(())
    }

    /// Lists every root in Steam's own format, in the primary root.
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
        let mut root = Value::block();
        for (index, folder) in self.all_roots().enumerate() {
            let mut entry = Value::block();
            entry.insert("path", folder.to_string_lossy().into_owned());
            entry.insert("label", "");
            root.insert(index.to_string(), entry);
        }
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

/// Where `game_id`'s manifest goes in `root`, or `None` if either id can't
/// be used in a path.
fn manifest_path_in(root: &Path, account_id: &str, storefront: Storefront, game_id: &str) -> Option<PathBuf> {
    let account = safe_component(account_id)?;
    let file_name = InstalledGame::manifest_file_name(storefront, game_id)?;
    Some(root.join("manifests").join(account).join(file_name))
}

/// Every manifest in one root, across every account.
fn scan_root(root: &Path) -> Vec<InstalledGame> {
    let dir = root.join("manifests");
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
    entries
        .flatten()
        .flat_map(|entry| read_manifest_dir(&entry.path()))
        .collect()
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
    let never = AtomicBool::new(false);
    copy_dir_recursive(src, dst, &mut bytes_copied, total_bytes, &never, &mut on_progress)?;
    fs::remove_dir_all(src)?;
    tracing::info!(from = %src.display(), to = %dst.display(), total_bytes, "moved directory (copy)");
    Ok(())
}

/// Copies `src` to `dst`, keeping symlinks as links, and leaves `src` as
/// it was. A failure partway, or `stop` turning, leaves a partial `dst`
/// for the caller to clear. `stop` is read between files, so a large file
/// already being copied still finishes first.
///
/// # Errors
/// Any filesystem operation along the way failing, or
/// [`io::ErrorKind::Interrupted`] once `stop` is set.
#[inline]
pub fn copy_dir(
    src: &Path,
    dst: &Path,
    stop: &AtomicBool,
    mut on_progress: impl FnMut(MoveProgress),
) -> io::Result<()> {
    let total_bytes = directory_size(src)?;
    let mut bytes_copied = 0u64;
    copy_dir_recursive(src, dst, &mut bytes_copied, total_bytes, stop, &mut on_progress)?;
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
    stop: &AtomicBool,
    on_progress: &mut impl FnMut(MoveProgress),
) -> io::Result<()> {
    fs::create_dir_all(dst)?;
    for entry in fs::read_dir(src)? {
        if stop.load(Ordering::Relaxed) {
            return Err(io::Error::new(io::ErrorKind::Interrupted, "copy stopped"));
        }
        let entry = entry?;
        let dst_path = dst.join(entry.file_name());
        let kind = entry.file_type()?;
        if kind.is_symlink() {
            // Recreated as a link: following it copied what it points at —
            // a Wine prefix's `dosdevices/z:` is `/` — or failed outright
            // on a link to a directory, halfway through the move.
            copy_symlink(&entry.path(), &dst_path)?;
        } else if kind.is_dir() {
            copy_dir_recursive(&entry.path(), &dst_path, bytes_copied, total_bytes, stop, on_progress)?;
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

/// Windows tells file links from directory links, so the kind follows what
/// the link points at now; a dangling link is recreated as a file link.
fn copy_symlink(link: &Path, dst: &Path) -> io::Result<()> {
    let target = fs::read_link(link)?;
    if fs::metadata(link).is_ok_and(|metadata| metadata.is_dir()) {
        platform::symlink_dir(&target, dst)
    } else {
        platform::symlink_file(&target, dst)
    }
}

/// A single path component that can't climb, hide, or reach an absolute
/// path. Every value that reaches this came from a storefront's manifest
/// or from an account id.
pub(crate) fn safe_component(value: &str) -> Option<&str> {
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
