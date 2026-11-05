//! What the Steam compatibility switches need to remember: which entries of Steam's
//! `libraryfolders.vdf` decanter added, since that file has no room to mark them, which Steam
//! library folders and games the user took out of a profile, so finding them again doesn't put
//! them back, and which of a profile's library folders are Steam's own.

use std::collections::HashSet;
use std::path::PathBuf;

use rusqlite::params;

use super::{Db, Result};

/// What the Steam compatibility switches leave automatic: each sub-switch only while the master
/// one is on too. The explicit actions ("Add to Steam", "Add through Steam") ignore these.
#[derive(Clone, Copy)]
pub struct SteamAutomatic {
    pub register_folders: bool,
    pub export_shortcuts: bool,
    pub discover: bool,
}

/// What the Steam compatibility switches remember of a profile's Steam library folders and games.
#[derive(Clone, Copy)]
pub enum SteamMark {
    /// A library root (`<steam library folder>/steamapps`) the user took out of the profile.
    RemovedFolder,
    /// A Steam app id whose entry the user took out of the profile.
    RemovedApp,
    /// A library root that is one of Steam's own library folders, which decanter never lists in
    /// Steam's file itself: one the user takes out of Steam stays out.
    SteamFolder,
}

impl SteamMark {
    const fn key(self) -> &'static str {
        match self {
            Self::RemovedFolder => "removed_folder",
            Self::RemovedApp => "removed_app",
            Self::SteamFolder => "steam_folder",
        }
    }
}

impl Db {
    /// Whether decanter works alongside the Steam on this machine at all — on by default; off,
    /// none of [`SteamAutomatic`]'s parts happens by itself.
    pub fn steam_compat_enabled(&self) -> Result<bool> {
        let enabled = self.setting("steam_compat_enabled")?.as_deref() != Some("0");
        tracing::debug!(enabled, "read steam_compat_enabled setting");
        Ok(enabled)
    }

    pub fn set_steam_compat_enabled(&self, enabled: bool) -> Result<()> {
        self.set_setting("steam_compat_enabled", if enabled { "1" } else { "0" })
    }

    /// Whether decanter's library folders are listed in Steam's own `libraryfolders.vdf` — on by
    /// default, so Steam offers them as places to install.
    pub fn steam_register_folders_enabled(&self) -> Result<bool> {
        let enabled = self.setting("steam_register_folders")?.as_deref() != Some("0");
        tracing::debug!(enabled, "read steam_register_folders setting");
        Ok(enabled)
    }

    pub fn set_steam_register_folders_enabled(&self, enabled: bool) -> Result<()> {
        self.set_setting("steam_register_folders", if enabled { "1" } else { "0" })
    }

    /// Whether Steam's library folders become library folders of the profile, and its installed
    /// games Steam entries, without the user adding them — on by default.
    pub fn steam_discover_enabled(&self) -> Result<bool> {
        let enabled = self.setting("steam_discover")?.as_deref() != Some("0");
        tracing::debug!(enabled, "read steam_discover setting");
        Ok(enabled)
    }

    pub fn set_steam_discover_enabled(&self, enabled: bool) -> Result<()> {
        self.set_setting("steam_discover", if enabled { "1" } else { "0" })
    }

    pub fn steam_automatic(&self) -> Result<SteamAutomatic> {
        let master = self.steam_compat_enabled()?;
        Ok(SteamAutomatic {
            register_folders: master && self.steam_register_folders_enabled()?,
            export_shortcuts: master && self.steam_export_enabled()?,
            discover: master && self.steam_discover_enabled()?,
        })
    }

    /// The library folders decanter added to Steam's `libraryfolders.vdf`, while they may still
    /// be in it or in its staged copy.
    pub fn steam_registered_folders(&self) -> Result<Vec<PathBuf>> {
        let mut stmt = self
            .conn
            .prepare("SELECT path FROM steam_library_folders ORDER BY path")?;
        let paths = stmt
            .query_map([], |row| row.get::<_, String>(0))?
            .map(|path| path.map(PathBuf::from))
            .collect::<rusqlite::Result<Vec<_>>>()?;
        tracing::debug!(count = paths.len(), "read library folders decanter added to steam");
        Ok(paths)
    }

    /// Replaces the record of the library folders decanter added to Steam. A path that isn't
    /// UTF-8 is never one of them: Steam's file is text.
    pub fn set_steam_registered_folders(&self, paths: &[PathBuf]) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        tx.execute("DELETE FROM steam_library_folders", [])?;
        for path in paths.iter().filter_map(|path| path.to_str()) {
            tx.execute(
                "INSERT OR IGNORE INTO steam_library_folders (path) VALUES (?1)",
                params![path],
            )?;
        }
        tx.commit()?;
        tracing::debug!(count = paths.len(), "recorded library folders decanter added to steam");
        Ok(())
    }

    pub fn steam_marked(&self, profile_id: &str, mark: SteamMark) -> Result<HashSet<String>> {
        let mut stmt = self
            .conn
            .prepare("SELECT item FROM steam_marks WHERE profile_id = ?1 AND kind = ?2")?;
        let items = stmt
            .query_map(params![profile_id, mark.key()], |row| row.get(0))?
            .collect::<rusqlite::Result<HashSet<String>>>()?;
        tracing::debug!(
            profile_id,
            mark = mark.key(),
            count = items.len(),
            "read marked steam items"
        );
        Ok(items)
    }

    /// The items marked `mark` in any profile.
    pub fn steam_marked_anywhere(&self, mark: SteamMark) -> Result<HashSet<String>> {
        let mut stmt = self.conn.prepare("SELECT item FROM steam_marks WHERE kind = ?1")?;
        let items = stmt
            .query_map(params![mark.key()], |row| row.get(0))?
            .collect::<rusqlite::Result<HashSet<String>>>()?;
        tracing::debug!(
            mark = mark.key(),
            count = items.len(),
            "read marked steam items in every profile"
        );
        Ok(items)
    }

    pub fn set_steam_marked(&self, profile_id: &str, mark: SteamMark, item: &str, marked: bool) -> Result<()> {
        if marked {
            self.conn.execute(
                "INSERT OR IGNORE INTO steam_marks (profile_id, kind, item) VALUES (?1, ?2, ?3)",
                params![profile_id, mark.key(), item],
            )?;
        } else {
            self.conn.execute(
                "DELETE FROM steam_marks WHERE profile_id = ?1 AND kind = ?2 AND item = ?3",
                params![profile_id, mark.key(), item],
            )?;
        }
        tracing::debug!(profile_id, mark = mark.key(), item, marked, "set steam item mark");
        Ok(())
    }
}
