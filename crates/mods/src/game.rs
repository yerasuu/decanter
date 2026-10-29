//! One game's mods folder and what each account does with it.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::manifest::ModManifest;
use crate::merge::{Owner, merge};
use crate::path::{ModId, ModPath, id_component};
use crate::set::ModSet;
use crate::tree::{self, Prepared};
use crate::{Error, Result, import};

const PROFILES: &str = "profiles";
const PLAY: &str = "play";
pub(crate) const INCOMING: &str = "incoming";

/// Where a game's manifest is filed, which is also the first level of its mods folder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Store {
    Gog,
    External,
}

impl Store {
    const fn folder(self) -> &'static str {
        match self {
            Self::Gog => "gog",
            Self::External => "ext",
        }
    }
}

/// The mods folder of one installed game: `<root>/decanter/mods/<store>/<game id>/`, under the root
/// whose `common/` holds the install, so links stay on one filesystem and move with the library.
#[derive(Debug, Clone)]
pub struct GameMods {
    pub(crate) dir: PathBuf,
    pub(crate) install: PathBuf,
    /// The install's folder name, as the play tree's links spell it.
    pub(crate) install_name: String,
}

/// How one enabled mod's files meet the game's and the other enabled mods', in load order.
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct ModConflicts {
    pub overrides_game: Vec<ModPath>,
    /// Per other mod, the paths this one wins over it.
    pub overrides: Vec<(ModId, Vec<ModPath>)>,
    /// Per other mod, the paths it wins over this one.
    pub overridden_by: Vec<(ModId, Vec<ModPath>)>,
}

impl GameMods {
    /// `install` is the game's `<root>/common/<InstallDir>`.
    ///
    /// # Errors
    ///
    /// [`Error::NotAnInstall`] when `install` is not a folder of a library's `common/`, and
    /// [`Error::InvalidName`] for a game id that can't name a folder.
    #[inline]
    pub fn new(install: &Path, store: Store, game_id: &str) -> Result<Self> {
        let not_an_install = || Error::NotAnInstall(install.display().to_string());
        let install_name = install
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(not_an_install)?;
        // A single component: the links climb to the root and back down through it.
        if install_name.contains('/') || ModPath::parse(install_name).is_err() {
            return Err(not_an_install());
        }
        let common = install.parent().ok_or_else(not_an_install)?;
        if common.file_name().and_then(|name| name.to_str()) != Some("common") {
            return Err(not_an_install());
        }
        let root = common.parent().ok_or_else(not_an_install)?;
        let dir = root
            .join("decanter")
            .join("mods")
            .join(store.folder())
            .join(id_component(game_id)?);
        Ok(Self {
            dir,
            install: install.to_path_buf(),
            install_name: install_name.to_owned(),
        })
    }

    #[must_use]
    #[inline]
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// The install the play trees mirror.
    #[must_use]
    #[inline]
    pub fn install(&self) -> &Path {
        &self.install
    }

    pub(crate) fn mod_dir(&self, id: &ModId) -> PathBuf {
        self.dir.join(id.as_str())
    }

    fn set_path(&self, account: &str) -> Result<PathBuf> {
        Ok(self.dir.join(PROFILES).join(format!("{}.toml", id_component(account)?)))
    }

    pub(crate) fn play_dir(&self, account: &str) -> Result<PathBuf> {
        Ok(self.dir.join(PLAY).join(id_component(account)?))
    }

    pub(crate) fn record_path(&self, account: &str) -> Result<PathBuf> {
        Ok(self.dir.join(PLAY).join(format!("{}.toml", id_component(account)?)))
    }

    /// Every mod of the game, whichever account added it. One whose `mod.toml` does not hold up is
    /// left out with a warning rather than hiding the rest.
    ///
    /// # Errors
    ///
    /// When the mods folder can't be read.
    #[inline]
    pub fn list(&self) -> Result<Vec<ModManifest>> {
        let entries = match fs::read_dir(&self.dir) {
            Ok(entries) => entries,
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(err) => return Err(Error::io(&self.dir)(err)),
        };
        let mut mods = Vec::new();
        for entry in entries {
            let entry = entry.map_err(Error::io(&self.dir))?;
            let Some(id) = entry.file_name().to_str().and_then(|name| ModId::parse(name).ok()) else {
                continue;
            };
            match ModManifest::read(&entry.path(), &id) {
                Ok(Some(manifest)) => mods.push(manifest),
                Ok(None) => tracing::debug!(path = %entry.path().display(), "mod folder without mod.toml, skipped"),
                Err(err) => tracing::warn!(%err, "mod left out, its mod.toml does not hold up"),
            }
        }
        mods.sort_by_key(|manifest| manifest.added_at);
        Ok(mods)
    }

    /// The mods `account` plays with, in load order.
    ///
    /// # Errors
    ///
    /// When its set file can't be read or does not hold up.
    #[inline]
    pub fn enabled(&self, account: &str) -> Result<Vec<ModId>> {
        Ok(ModSet::read(&self.set_path(account)?)?.mods)
    }

    /// Turns a mod on or off for `account`; one turned on goes last in the load order.
    ///
    /// # Errors
    ///
    /// [`Error::UnknownMod`] for a mod the game does not have, else when the set file can't be
    /// read or written.
    #[inline]
    pub fn set_enabled(&self, account: &str, id: &ModId, enabled: bool) -> Result<()> {
        if !self.mod_dir(id).is_dir() {
            return Err(Error::UnknownMod(id.to_string()));
        }
        let path = self.set_path(account)?;
        let mut set = ModSet::read(&path)?;
        set.set_enabled(id, enabled);
        set.write(&path)?;
        tracing::info!(account, mod_id = %id, enabled, "mod set changed");
        Ok(())
    }

    /// Copies a folder of loose files laid out as the game's root in as a new mod, enabled for
    /// `account`. The folder is only read.
    ///
    /// # Errors
    ///
    /// When the folder breaks a rule of a mod (a link or device inside it, a reserved name, two
    /// names that are one file for a Windows game, the size and file caps), when the library's disk
    /// lacks the space, or on any read or write failure; nothing is left behind then.
    #[inline]
    pub fn import_folder(&self, source: &Path, account: &str) -> Result<ModManifest> {
        let path = self.set_path(account)?;
        let manifest = import::folder(self, source)?;
        let mut set = ModSet::read(&path)?;
        set.set_enabled(&manifest.id, true);
        set.write(&path)?;
        Ok(manifest)
    }

    /// The accounts whose set has `id` enabled, which removing it takes it from.
    ///
    /// # Errors
    ///
    /// When a set file can't be read or does not hold up.
    #[inline]
    pub fn users(&self, id: &ModId) -> Result<Vec<String>> {
        let mut users = Vec::new();
        for (account, set) in self.sets()? {
            if set.is_enabled(id) {
                users.push(account);
            }
        }
        Ok(users)
    }

    /// Takes the mod out of every account's set, then deletes its folder. The play trees' links to
    /// it are replaced at each account's next launch.
    ///
    /// # Errors
    ///
    /// [`Error::UnknownMod`] for a mod the game does not have, else when a set file or the mod's
    /// folder can't be changed.
    #[inline]
    pub fn remove(&self, id: &ModId) -> Result<()> {
        let dir = self.mod_dir(id);
        if !dir.is_dir() {
            return Err(Error::UnknownMod(id.to_string()));
        }
        for (account, mut set) in self.sets()? {
            set.forget(id);
            set.write(&self.set_path(&account)?)?;
        }
        // Renamed away first so that a deletion cut short leaves no half mod under its id.
        let incoming = self.dir.join(INCOMING);
        fs::create_dir_all(&incoming).map_err(Error::io(&incoming))?;
        let doomed = incoming.join(format!("removed-{id}-{}", std::process::id()));
        fs::rename(&dir, &doomed).map_err(Error::io(&dir))?;
        fs::remove_dir_all(&doomed).map_err(Error::io(&doomed))?;
        tracing::info!(mod_id = %id, "mod removed");
        Ok(())
    }

    fn sets(&self) -> Result<Vec<(String, ModSet)>> {
        let dir = self.dir.join(PROFILES);
        let entries = match fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(err) => return Err(Error::io(&dir)(err)),
        };
        let mut sets = Vec::new();
        for entry in entries {
            let entry = entry.map_err(Error::io(&dir))?;
            let name = entry.file_name();
            let Some(account) = name
                .to_str()
                .and_then(|name| name.strip_suffix(".toml"))
                .and_then(|name| id_component(name).ok())
            else {
                continue;
            };
            sets.push((account.to_owned(), ModSet::read(&entry.path())?));
        }
        Ok(sets)
    }

    /// The enabled mods of `account`, in load order; a removed one is skipped, a broken one fails.
    fn enabled_manifests(&self, account: &str) -> Result<Vec<ModManifest>> {
        let set = ModSet::read(&self.set_path(account)?)?;
        let mut mods = Vec::with_capacity(set.mods.len());
        for id in &set.mods {
            if let Some(manifest) = ModManifest::read(&self.mod_dir(id), id)? {
                mods.push(manifest);
            } else {
                tracing::debug!(mod_id = %id, "enabled mod no longer exists, skipped");
            }
        }
        Ok(mods)
    }

    /// Per enabled mod of `account`, what it overrides and what overrides it.
    ///
    /// # Errors
    ///
    /// When the install can't be walked or an enabled mod's `mod.toml` does not hold up.
    #[inline]
    pub fn conflicts(&self, account: &str) -> Result<BTreeMap<ModId, ModConflicts>> {
        let mods = self.enabled_manifests(account)?;
        let merged = merge(self, &mods)?;
        let mut found = vec![ModConflicts::default(); mods.len()];
        let mut overrides = vec![BTreeMap::<usize, Vec<ModPath>>::new(); mods.len()];
        let mut overridden_by = vec![BTreeMap::<usize, Vec<ModPath>>::new(); mods.len()];
        for wanted in merged.values() {
            let Some((&Owner::Mod(winner), losers)) = wanted.providers.split_last() else {
                continue;
            };
            for provider in losers {
                match *provider {
                    Owner::Game => {
                        if let Some(conflicts) = found.get_mut(winner) {
                            conflicts.overrides_game.push(wanted.path.clone());
                        }
                    }
                    Owner::Mod(loser) if loser != winner => {
                        if let Some(by_mod) = overrides.get_mut(winner) {
                            by_mod.entry(loser).or_default().push(wanted.path.clone());
                        }
                        if let Some(by_mod) = overridden_by.get_mut(loser) {
                            by_mod.entry(winner).or_default().push(wanted.path.clone());
                        }
                    }
                    Owner::Mod(_) => {}
                }
            }
        }
        let id_of = |index: usize| mods.get(index).map(|manifest| manifest.id.clone());
        let named = |by_mod: BTreeMap<usize, Vec<ModPath>>| {
            by_mod
                .into_iter()
                .filter_map(|(index, paths)| id_of(index).map(|id| (id, paths)))
                .collect()
        };
        Ok(mods
            .iter()
            .zip(found)
            .zip(overrides.into_iter().zip(overridden_by))
            .map(|((manifest, mut conflicts), (wins, losses))| {
                conflicts.overrides = named(wins);
                conflicts.overridden_by = named(losses);
                (manifest.id.clone(), conflicts)
            })
            .collect())
    }

    /// Brings `account`'s play tree up to date with its set and returns where the game starts
    /// from; `None` while the account has never enabled a mod, so the game starts from its install
    /// as it always did. Must not run while the game runs: links it replaces may be open.
    ///
    /// # Errors
    ///
    /// When an enabled mod does not hold up (its `mod.toml`, or a file that is missing or leads
    /// outside it), or a link can't be placed; the launch then fails rather than starting the
    /// game without its mods.
    #[inline]
    pub fn prepare(&self, account: &str) -> Result<Option<Prepared>> {
        let mods = self.enabled_manifests(account)?;
        let play = self.play_dir(account)?;
        if mods.is_empty() && !play.exists() {
            return Ok(None);
        }
        tree::reconcile(self, account, &mods).map(Some)
    }

    /// The files in `account`'s play tree that decanter did not place: the game's saves and
    /// rewritten configs, which win over the install and every mod.
    ///
    /// # Errors
    ///
    /// When the play tree or its record can't be read.
    #[inline]
    pub fn written_by_game(&self, account: &str) -> Result<Vec<ModPath>> {
        tree::written_by_game(self, account)
    }
}
