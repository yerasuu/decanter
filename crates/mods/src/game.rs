//! One game's mods folder and what each account does with it.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::archive::{self, ArchivePreview};
use crate::base;
use crate::dll::{self, DllMode, EffectiveOverride};
use crate::manifest::ModManifest;
use crate::merge::{Owner, merge};
use crate::path::{ModId, ModPath, id_component};
use crate::progress::Progress;
use crate::set::{ModSet, Shift};
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

    /// Moves an enabled mod one place in `account`'s load order, where the later one wins a file and
    /// a DLL override.
    ///
    /// # Errors
    ///
    /// [`Error::UnknownMod`] for a mod the game does not have, else when the set file can't be
    /// read or written.
    #[inline]
    pub fn shift(&self, account: &str, id: &ModId, step: Shift) -> Result<()> {
        if !self.mod_dir(id).is_dir() {
            return Err(Error::UnknownMod(id.to_string()));
        }
        let path = self.set_path(account)?;
        let mut set = ModSet::read(&path)?;
        if set.shift(id, step) {
            set.write(&path)?;
            tracing::info!(account, mod_id = %id, ?step, "mod load order changed");
        }
        Ok(())
    }

    /// Changes how Wine loads `dll` for the mod, for every account that has it on; a mod that did
    /// not ask for `dll` gets it added.
    ///
    /// # Errors
    ///
    /// [`Error::UnknownMod`] for a mod the game does not have, [`Error::InvalidOverride`] for a
    /// name that can't be a DLL's, else when its `mod.toml` can't be read or written.
    #[inline]
    pub fn set_dll_mode(&self, id: &ModId, dll: &str, mode: DllMode) -> Result<()> {
        let dir = self.mod_dir(id);
        let mut manifest = ModManifest::read(&dir, id)?.ok_or_else(|| Error::UnknownMod(id.to_string()))?;
        let wanted = dll::DllOverride::new(dll, mode)?;
        if let Some(known) = manifest.dll_overrides.iter_mut().find(|known| known.dll == wanted.dll) {
            known.mode = mode;
        } else {
            manifest.dll_overrides.push(wanted);
        }
        manifest.write(&dir)?;
        tracing::info!(mod_id = %id, dll, mode = mode.as_str(), "mod dll override changed");
        Ok(())
    }

    /// The DLL overrides `account`'s enabled mods ask for, the later mod winning one asked for
    /// twice.
    ///
    /// # Errors
    ///
    /// When the set file or an enabled mod's `mod.toml` does not hold up.
    #[inline]
    pub fn dll_overrides(&self, account: &str) -> Result<Vec<EffectiveOverride>> {
        Ok(dll::effective(&self.enabled_manifests(account)?))
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

    /// Reads a zip or 7z archive the user gave, hashing it and checking every entry, without
    /// writing anything; [`Self::import_archive`] then extracts what it read. The file is only read.
    ///
    /// # Errors
    ///
    /// When the file is no archive decanter reads (RAR among them), is encrypted, or holds an
    /// entry that breaks a rule of a mod (a name that leaves the mod, a link or device, a
    /// reserved name, two names that are one file for a Windows game, the entry, size, file and
    /// ratio caps), [`Error::Cancelled`] when `progress` was cancelled, or on a read failure.
    #[inline]
    pub fn preview_archive(&self, source: &Path, progress: &Progress) -> Result<ArchivePreview> {
        archive::preview(self, source, progress)
    }

    /// Extracts the archive `preview` read into a new mod, enabled for `account`, named `name` and
    /// with its files under `target` (`None` for the game's root).
    ///
    /// # Errors
    ///
    /// [`Error::SourceChanged`] when the file changed since the preview, [`Error::Cancelled`] when
    /// `progress` was cancelled, else as [`Self::preview_archive`], when the library's disk lacks
    /// the space, or on any read or write failure; nothing is left behind then.
    #[inline]
    pub fn import_archive(
        &self,
        preview: &ArchivePreview,
        name: &str,
        target: Option<ModPath>,
        account: &str,
        progress: &Progress,
    ) -> Result<ModManifest> {
        let path = self.set_path(account)?;
        let manifest = archive::import(self, preview, name, target, progress)?;
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
    /// The game's files the enabled mods touch are first compared with what they were when the
    /// mods were applied, once per `build_id` (the installed build); a mod an update changed a file
    /// under is left out until [`Self::keep`] or it is disabled.
    ///
    /// # Errors
    ///
    /// When an enabled mod does not hold up (its `mod.toml`, or a file that is missing or leads
    /// outside it), or a link can't be placed; the launch then fails rather than starting the
    /// game without its mods.
    #[inline]
    pub fn prepare(&self, account: &str, build_id: &str) -> Result<Option<Prepared>> {
        let mut mods = self.enabled_manifests(account)?;
        let play = self.play_dir(account)?;
        if mods.is_empty() && !play.exists() {
            return Ok(None);
        }
        let set = self.checked_set(account, build_id, &mods)?;
        let left_out: Vec<(ModId, String)> = mods
            .iter()
            .filter(|manifest| set.attention.contains_key(&manifest.id))
            .map(|manifest| (manifest.id.clone(), manifest.name.clone()))
            .collect();
        mods.retain(|manifest| !set.attention.contains_key(&manifest.id));
        for skipped in &left_out {
            tracing::warn!(account, mod_id = %skipped.0, name = skipped.1, "mod left out of the play tree, it needs attention");
        }
        let mut prepared = tree::reconcile(self, account, &mods)?;
        prepared.left_out = left_out;
        Ok(Some(prepared))
    }

    /// `account`'s set after comparing the files its enabled `mods` touch at `build_id`, written
    /// back when that changed it.
    fn checked_set(&self, account: &str, build_id: &str, mods: &[ModManifest]) -> Result<ModSet> {
        let path = self.set_path(account)?;
        let mut set = ModSet::read(&path)?;
        if !mods.is_empty() && base::check(self, &mut set, build_id, mods)? {
            set.write(&path)?;
        }
        Ok(set)
    }

    /// The enabled mods of `account` that an update changed a game file under, with those files,
    /// comparing them first when `build_id` is new; what the Mods tab and the Updates tab show.
    ///
    /// # Errors
    ///
    /// When the set file or an enabled mod does not hold up, or a game file can't be read.
    #[inline]
    pub fn needing_attention(&self, account: &str, build_id: &str) -> Result<BTreeMap<ModId, Vec<ModPath>>> {
        let mods = self.enabled_manifests(account)?;
        Ok(self.checked_set(account, build_id, &mods)?.attention)
    }

    /// Takes the game's files as they are now as what `id` was made for, which lets it back into
    /// `account`'s play tree.
    ///
    /// # Errors
    ///
    /// When the set file can't be read or written, or a game file can't be read.
    #[inline]
    pub fn keep(&self, account: &str, id: &ModId) -> Result<()> {
        let path = self.set_path(account)?;
        let mut set = ModSet::read(&path)?;
        base::keep(self, &mut set, id)?;
        set.write(&path)
    }

    /// The mod's files that are no longer what was imported (a game that rewrote one in place).
    ///
    /// # Errors
    ///
    /// [`Error::UnknownMod`] for a mod the game does not have, else when a file can't be read.
    #[inline]
    pub fn changed_files(&self, id: &ModId) -> Result<Vec<ModPath>> {
        let manifest = ModManifest::read(&self.mod_dir(id), id)?.ok_or_else(|| Error::UnknownMod(id.to_string()))?;
        base::changed_files(self, &manifest)
    }

    /// The bytes of what the game wrote into `account`'s play tree, which go with it on uninstall;
    /// the links themselves are not counted.
    ///
    /// # Errors
    ///
    /// When the play tree can't be read.
    #[inline]
    pub fn play_size(&self, account: &str) -> Result<u64> {
        tree::real_size(&self.play_dir(account)?)
    }

    /// Deletes `account`'s play tree and its record, as uninstalling the game does with its prefix;
    /// its set stays, so a reinstall plays with the same mods.
    ///
    /// # Errors
    ///
    /// When either can't be deleted.
    #[inline]
    pub fn remove_play(&self, account: &str) -> Result<()> {
        let play = self.play_dir(account)?;
        match fs::remove_dir_all(&play) {
            Ok(()) => {}
            Err(err) if err.kind() == io::ErrorKind::NotFound => {}
            Err(err) => return Err(Error::io(&play)(err)),
        }
        let record = self.record_path(account)?;
        match fs::remove_file(&record) {
            Ok(()) => {}
            Err(err) if err.kind() == io::ErrorKind::NotFound => {}
            Err(err) => return Err(Error::io(&record)(err)),
        }
        tracing::info!(account, path = %play.display(), "play tree removed");
        Ok(())
    }

    /// Deletes the game's whole mods folder: every mod, set and play tree.
    ///
    /// # Errors
    ///
    /// When it can't be deleted; renamed away first, so a deletion cut short leaves no half mods
    /// folder under the game's id.
    #[inline]
    pub fn remove_all(&self) -> Result<()> {
        let Some(parent) = self.dir.parent() else {
            return Err(Error::NotAnInstall(self.dir.display().to_string()));
        };
        let name = self.dir.file_name().and_then(|name| name.to_str()).unwrap_or("game");
        let doomed = parent.join(format!(".{name}.removed-{}", std::process::id()));
        match fs::rename(&self.dir, &doomed) {
            Ok(()) => {}
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(err) => return Err(Error::io(&self.dir)(err)),
        }
        fs::remove_dir_all(&doomed).map_err(Error::io(&doomed))?;
        tracing::info!(path = %self.dir.display(), "game's mods folder removed");
        Ok(())
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
