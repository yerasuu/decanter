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

use super::LauncherSettings;
use super::icons::{self, COMMON_TPF, IconStyle};
use super::pad;
use super::tools::ModProfile;

/// decanter's own mod in the folder bbport sees; last alphabetically too,
/// should anything ignore the order.
const ICONS_MOD: &str = "zz-decanter-iconos";

/// What bbport is given as `BB_MODS_DIR`, `BB_MODS_CONFIG` and
/// `BB_MODS_ENABLED`.
pub struct Mods {
    pub dir: PathBuf,
    pub config: PathBuf,
    pub enabled: bool,
}

impl Mods {
    /// bbport's mods folder with mods switched off: the mods and the icons
    /// mod are Bloodborne's files.
    pub fn off(settings: &LauncherSettings) -> Self {
        Self {
            dir: user_dir(settings),
            config: ModProfile::path(),
            enabled: false,
        }
    }
}

fn user_dir(settings: &LauncherSettings) -> PathBuf {
    settings
        .text("mods_dir")
        .map_or_else(|| crate::paths::bbport_data().join("mods"), PathBuf::from)
}

/// The user's mods by folder name, in their load order.
fn user_mods(settings: &LauncherSettings) -> (PathBuf, Vec<String>, ModProfile) {
    let dir = user_dir(settings);
    let mut available: Vec<String> = fs::read_dir(&dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|entry| entry.path().is_dir())
        .filter_map(|entry| entry.file_name().into_string().ok())
        .filter(|name| name != ICONS_MOD)
        .collect();
    available.sort();
    let profile = ModProfile::load(&ModProfile::path());
    let ordered = profile.ordered(&available);
    (dir, ordered, profile)
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
fn icon_base(game_dir: &Path, settings: &LauncherSettings) -> PathBuf {
    let (dir, ordered, profile) = user_mods(settings);
    let enabled = settings.flag("mods_enabled", true);
    ordered
        .iter()
        .rev()
        .filter(|_| enabled)
        .filter(|name| !profile.disabled.contains(name))
        .map(|name| dir.join(name).join(COMMON_TPF))
        .find(|file| file.is_file())
        .unwrap_or_else(|| game_dir.join(COMMON_TPF))
}

/// The folder and order bbport loads mods from. Without the icons, the
/// user's own as they are. `detected` is the style of the pad in use, for
/// the automatic setting. File I/O and the icons' build: off the UI thread.
pub fn prepare(game_dir: &Path, settings: &LauncherSettings, detected: Option<IconStyle>) -> io::Result<Mods> {
    let enabled = settings.flag("mods_enabled", true);
    let Some(style) = icon_style(settings, detected) else {
        return Ok(Mods {
            dir: user_dir(settings),
            config: ModProfile::path(),
            enabled,
        });
    };
    let icons = match icons::prepare(&pad::from_ini(), style, &icon_base(game_dir, settings)) {
        Ok(icons) => icons,
        Err(err) => return Err(err),
    };
    let (dir, ordered, profile) = user_mods(settings);
    let view = crate::paths::bbport_data().join("mods-effective");
    // Only links live here; removing it never touches what they point at.
    if let Err(err) = fs::remove_dir_all(&view)
        && err.kind() != io::ErrorKind::NotFound
    {
        return Err(err);
    }
    fs::create_dir_all(&view)?;
    for name in &ordered {
        std::os::unix::fs::symlink(dir.join(name), view.join(name))?;
    }
    std::os::unix::fs::symlink(&icons, view.join(ICONS_MOD))?;
    let disabled = if enabled { profile.disabled } else { ordered.clone() };
    let mut order = ordered;
    order.push(ICONS_MOD.to_owned());
    let config = crate::paths::bbport_data().join("mods-effective.json");
    ModProfile { order, disabled }.save(&config)?;
    Ok(Mods {
        dir: view,
        config,
        enabled: true,
    })
}
