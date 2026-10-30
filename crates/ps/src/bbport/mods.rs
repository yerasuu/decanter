//! The mods folder bbport is pointed at.
//!
//! With the layout's button icons on, decanter adds a mod of its own that
//! the user never sees in their mods folder or list: bbport is given a
//! folder of links to the user's mods plus decanter's, and a load order
//! with decanter's last. The user's choices carry over as they are; with
//! their mods switched off, theirs are listed as disabled and only
//! decanter's loads.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use decanter_core::platform::link_dir;

use super::icons::{self, COMMON_TPF, IconStyle};
use super::tools::{MODS_FILE, ModProfile, PATCHES_FILE};
use super::{IniScope, LauncherSettings, Names, pad, port_ini};
use crate::paths::Root;
use crate::port::Port;

/// decanter's own mod in the folder bbport sees; last alphabetically too,
/// should anything ignore the order.
const ICONS_MOD: &str = "zz-decanter-iconos";

/// The launch settings naming a title's mods and patches folders, in that order.
pub const FOLDER_KEYS: [&str; 2] = ["mods_dir", "patches_dir"];

/// The folders [`FOLDER_KEYS`] default to, inside a title's own folder or the shared one.
const FOLDER_NAMES: [&str; 2] = ["mods", "patches"];

/// Where a title's mods and patches are read from and its choices among them kept: its own
/// folder ([`super::Bbport::title_ini`]'s scope), or the shared data folder where every title
/// shares its ini.
pub struct ModFiles {
    pub mods_dir: PathBuf,
    pub patches_dir: PathBuf,
    /// `mods.json`, the load order and the mods switched off.
    pub mods_config: PathBuf,
    /// `patches.json`, the patch switches.
    pub patches_config: PathBuf,
    pub scope: IniScope,
    /// The title's own folder, or the shared one: where the launch's links to its mods go.
    pub folder: PathBuf,
}

impl ModFiles {
    /// `title_id`'s, with its `settings` ([`LauncherSettings::of_title`]); its `mods.json` and
    /// `patches.json` are copies of the shared ones the first time it is asked for. File I/O: off
    /// the UI thread.
    ///
    /// # Errors
    /// Backing up the shared files before the first title takes its copies, or writing a copy.
    pub fn of_title(root: &Root, title_id: &str, settings: &LauncherSettings) -> io::Result<Self> {
        let names = Names::of_blocking(&Port::Bbport.checkout(root));
        let (folder, scope) = match port_ini::own_title(names, title_id) {
            Ok(id) => {
                port_ini::title_copy(root, &id, MODS_FILE)?;
                port_ini::title_copy(root, &id, PATCHES_FILE)?;
                (root.title(&id), IniScope::Title)
            }
            Err(scope) => (root.bbport_data(), scope),
        };
        let ([mods_key, patches_key], [mods_name, patches_name]) = (FOLDER_KEYS, FOLDER_NAMES);
        let dir = |key: &str, name: &str| settings.text(key).map_or_else(|| folder.join(name), PathBuf::from);
        let patches_dir = dir(patches_key, patches_name);
        if settings.text(patches_key).is_none()
            && let Err(err) = super::builtin_patches::seed(&patches_dir, title_id)
        {
            tracing::warn!(dir = %patches_dir.display(), %err, "could not seed the title's patches folder");
        }
        Ok(Self {
            mods_dir: dir(mods_key, mods_name),
            patches_dir,
            mods_config: folder.join(MODS_FILE),
            patches_config: folder.join(PATCHES_FILE),
            scope,
            folder,
        })
    }
}

/// What a title read `key` ([`FOLDER_KEYS`]) as before it had folders of its own, where it has no
/// setting of its own: the shared data folder's when it is there, so mods a user already put in it
/// keep loading. `None` for another key or a path that isn't UTF-8.
pub(super) fn former_folder(root: &Root, key: &str) -> Option<String> {
    let name = FOLDER_KEYS
        .into_iter()
        .zip(FOLDER_NAMES)
        .find_map(|(folder_key, name)| (folder_key == key).then_some(name))?;
    let dir = root.bbport_data().join(name);
    dir.is_dir().then(|| dir.to_str().map(str::to_owned)).flatten()
}

/// What bbport is given as `PS_MODS_DIR`, `PS_MODS_CONFIG`, `PS_MODS_ENABLED`, `PS_PATCHES_DIR`
/// and `PS_PATCHES_CONFIG`.
pub struct Mods {
    pub dir: PathBuf,
    pub config: PathBuf,
    pub enabled: bool,
    pub patches_dir: PathBuf,
    pub patches_config: PathBuf,
}

impl Mods {
    /// bbport's mods folder with mods switched off: the mods and the icons
    /// mod are Bloodborne's files.
    #[must_use]
    pub fn off(files: &ModFiles) -> Self {
        Self::as_set(files, false)
    }

    fn as_set(files: &ModFiles, enabled: bool) -> Self {
        Self {
            dir: files.mods_dir.clone(),
            config: files.mods_config.clone(),
            enabled,
            patches_dir: files.patches_dir.clone(),
            patches_config: files.patches_config.clone(),
        }
    }
}

/// The user's mods by folder name, in their load order.
fn user_mods(files: &ModFiles) -> (Vec<String>, ModProfile) {
    let mut available: Vec<String> = fs::read_dir(&files.mods_dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|entry| entry.path().is_dir())
        .filter_map(|entry| entry.file_name().into_string().ok())
        .filter(|name| name != ICONS_MOD)
        .collect();
    available.sort();
    let profile = ModProfile::load(&files.mods_config);
    let ordered = profile.ordered(&available);
    (ordered, profile)
}

/// The icons the launch settings ask for: `None` for the game's own, a
/// fixed style, or by default the one `detected` from the pad in use.
pub fn icon_style(settings: &LauncherSettings, detected: Option<IconStyle>) -> Option<IconStyle> {
    match settings.text("button_icons") {
        Some("original") => None,
        Some(id) => Some(IconStyle::from_id(id).or(detected).unwrap_or(IconStyle::Abxy)),
        None => Some(detected.unwrap_or(IconStyle::Abxy)),
    }
}

/// The file the icons are drawn over: the last enabled mod's
/// `common.tpf.dcx` when one replaces it, else the game's.
fn icon_base(game_dir: &Path, settings: &LauncherSettings, files: &ModFiles) -> PathBuf {
    let (ordered, profile) = user_mods(files);
    let enabled = settings.flag("mods_enabled", true);
    ordered
        .iter()
        .rev()
        .filter(|_| enabled)
        .filter(|name| !profile.disabled.contains(name))
        .map(|name| files.mods_dir.join(name).join(COMMON_TPF))
        .find(|file| file.is_file())
        .unwrap_or_else(|| game_dir.join(COMMON_TPF))
}

/// The folder and order bbport loads mods from. Without the icons, the
/// user's own as they are. `detected` is the style of the pad in use, for
/// the automatic setting; `port_ini` is the title's, whose pad layout the icons follow. The links
/// and their order go in the title's own folder ([`ModFiles`]), so two titles started at once
/// never rebuild each other's. File I/O and the icons' build: off the UI thread.
pub fn prepare(
    root: &Root,
    game_dir: &Path,
    settings: &LauncherSettings,
    files: &ModFiles,
    port_ini: &Path,
    detected: Option<IconStyle>,
) -> io::Result<Mods> {
    let enabled = settings.flag("mods_enabled", true);
    let Some(style) = icon_style(settings, detected) else {
        return Ok(Mods::as_set(files, enabled));
    };
    let icons = match icons::prepare(
        root,
        &pad::from_ini(port_ini),
        style,
        &icon_base(game_dir, settings, files),
    ) {
        Ok(icons) => icons,
        Err(err) => return Err(err),
    };
    let (ordered, profile) = user_mods(files);
    let view = files.folder.join("mods-effective");
    // Only links live here; removing it never touches what they point at.
    if let Err(err) = fs::remove_dir_all(&view)
        && err.kind() != io::ErrorKind::NotFound
    {
        return Err(err);
    }
    fs::create_dir_all(&view)?;
    for name in &ordered {
        link_dir(&files.mods_dir.join(name), &view.join(name))?;
    }
    link_dir(&icons, &view.join(ICONS_MOD))?;
    let disabled = if enabled { profile.disabled } else { ordered.clone() };
    let mut order = ordered;
    order.push(ICONS_MOD.to_owned());
    let config = files.folder.join("mods-effective.json");
    ModProfile { order, disabled }.save(&config)?;
    Ok(Mods {
        dir: view,
        config,
        ..Mods::as_set(files, true)
    })
}
