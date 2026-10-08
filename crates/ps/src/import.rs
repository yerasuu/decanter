//! Bringing over what the standalone psport launcher (and bbport's own
//! launcher before it) left on disk, once, into decanter's layout.
//!
//! Copies only, never moves: the old files stay where they were, and
//! nothing already in decanter's layout is overwritten. Mod and patch
//! folders are not copied — they can be large — but pointed at where they
//! are. AnyPS5 conversions are not copied either: their links point into
//! the old checkout, and a game converts again on its own when played; its
//! bindings are kept.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::bbport::LauncherSettings;

/// What psport's `config.toml` held that still matters.
#[derive(Deserialize, Default)]
#[serde(default)]
struct Config {
    scan_dirs: Vec<PathBuf>,
    bbport_dir: Option<PathBuf>,
}

/// Where psport and bbport's launcher kept their files.
pub struct Sources {
    pub config: PathBuf,
    pub data: PathBuf,
    pub launcher_settings: PathBuf,
    /// psport's default bbport checkout, for a config that names none.
    pub default_bbport: PathBuf,
    /// psport's default game folder, for a psport that never saved a config.
    pub default_games: PathBuf,
}

impl Sources {
    pub fn from_home(home: &Path) -> Self {
        Self {
            config: home.join(".config/psport/config.toml"),
            data: home.join(".local/share/psport"),
            launcher_settings: home.join(".config/bbport-launcher/settings.json"),
            default_bbport: home.join("Projects/experiments/psport/bbport"),
            default_games: home.join("Games"),
        }
    }
}

pub struct Imported {
    /// psport's game folders, for the profile's own list.
    pub scan_dirs: Vec<PathBuf>,
    /// What was brought over, for the log.
    pub copied: Vec<PathBuf>,
}

/// Brings everything found into `profile_id`'s saves and failures and the
/// shared bbport data. File I/O over the old saves: off the UI thread.
pub fn import(sources: &Sources, profile_id: &str) -> io::Result<Imported> {
    let mut config: Config = fs::read_to_string(&sources.config)
        .ok()
        .and_then(|text| toml::from_str(&text).ok())
        .unwrap_or_default();
    // With no config, psport scanned its default folder; that it ran at
    // all shows in its data directory.
    if !sources.config.exists() && sources.data.is_dir() && sources.default_games.is_dir() {
        config.scan_dirs.push(sources.default_games.clone());
    }
    let bbport = config.bbport_dir.unwrap_or_else(|| sources.default_bbport.clone());
    let data = crate::paths::bbport_data();
    let profile = crate::paths::profile(profile_id);
    let mut copied = Vec::new();

    for name in ["bbport.ini", "mods.json", "patches.json"] {
        copy_new(&bbport.join(name), &data.join(name), &mut copied)?;
    }
    copy_new(&sources.launcher_settings, &data.join("settings.json"), &mut copied)?;
    // The launcher's own user_dir, when it had one, held the saves.
    let launch = LauncherSettings::load();
    let saves = launch
        .text("user_dir")
        .map_or_else(|| bbport.join("user"), PathBuf::from);
    copy_new(&saves, &profile.join("bbport/user"), &mut copied)?;
    copy_new(
        &sources.data.join("failures.jsonl"),
        &profile.join("failures.jsonl"),
        &mut copied,
    )?;
    copy_new(&sources.data.join("fsr4_411"), &crate::paths::fsr411(), &mut copied)?;
    if let Ok(entries) = fs::read_dir(sources.data.join("anyps5")) {
        for entry in entries.flatten() {
            let title = entry.file_name();
            let binds = entry.path().join("anyps5-input.ini");
            copy_new(
                &binds,
                &crate::paths::anyps5().join(&title).join("anyps5-input.ini"),
                &mut copied,
            )?;
        }
    }

    // Folders that were never chosen in the launcher lived in the old
    // checkout; decanter's default is its own data directory.
    let mut pointed = false;
    for (key, default) in [("mods_dir", "mods"), ("patches_dir", "patches")] {
        let old = bbport.join(default);
        if launch.text(key).is_none() && old.is_dir() {
            let old = old.to_string_lossy().into_owned();
            pointed = true;
            LauncherSettings::edit(|settings| settings.set_text(key, &old))?;
        }
    }
    if pointed {
        copied.push(data.join("settings.json"));
    }
    Ok(Imported {
        scan_dirs: config.scan_dirs,
        copied,
    })
}

/// `from` (a file or a whole directory) to `to`, unless `to` exists or
/// there is nothing at `from`.
fn copy_new(from: &Path, to: &Path, copied: &mut Vec<PathBuf>) -> io::Result<()> {
    if to.exists() || !from.exists() {
        return Ok(());
    }
    if let Some(parent) = to.parent() {
        fs::create_dir_all(parent)?;
    }
    if from.is_dir() {
        copy_dir(from, to)?;
    } else {
        fs::copy(from, to)?;
    }
    copied.push(to.to_owned());
    Ok(())
}

/// Symlinks are recreated as links, not followed: an old save directory
/// pointing elsewhere keeps pointing there.
fn copy_dir(from: &Path, to: &Path) -> io::Result<()> {
    fs::create_dir_all(to)?;
    for entry in fs::read_dir(from)? {
        let entry = entry?;
        let (source, dest) = (entry.path(), to.join(entry.file_name()));
        let kind = entry.file_type()?;
        if kind.is_symlink() {
            std::os::unix::fs::symlink(fs::read_link(&source)?, &dest)?;
        } else if kind.is_dir() {
            copy_dir(&source, &dest)?;
        } else {
            fs::copy(&source, &dest)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn copies_new_files_and_whole_directories_without_overwriting() {
        let root = std::env::temp_dir().join(format!("decanter-ps-import-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let (from, to) = (root.join("old"), root.join("new"));
        fs::create_dir_all(from.join("user/save")).unwrap();
        fs::write(from.join("user/save/slot0"), "old save").unwrap();
        fs::write(from.join("bbport.ini"), "upscaler=fsr3\n").unwrap();
        fs::create_dir_all(&to).unwrap();
        fs::write(to.join("bbport.ini"), "upscaler=fsr4\n").unwrap();

        let mut copied = Vec::new();
        copy_new(&from.join("user"), &to.join("user"), &mut copied).unwrap();
        copy_new(&from.join("bbport.ini"), &to.join("bbport.ini"), &mut copied).unwrap();
        copy_new(&from.join("missing"), &to.join("missing"), &mut copied).unwrap();

        assert_eq!(fs::read_to_string(to.join("user/save/slot0")).unwrap(), "old save");
        assert_eq!(fs::read_to_string(to.join("bbport.ini")).unwrap(), "upscaler=fsr4\n");
        assert_eq!(copied, [to.join("user")]);
        assert!(from.join("user/save/slot0").exists(), "the old files stay");
        fs::remove_dir_all(&root).unwrap();
    }
}
