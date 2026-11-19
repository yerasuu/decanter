//! Folders found in a profile's library folders that the user said aren't games to add, so the
//! External games page stops proposing them. Keyed by the folder's full path, as the scan finds it.

use std::collections::HashSet;
use std::path::Path;

use rusqlite::params;

use super::{Db, Result};

impl Db {
    pub fn dismiss_folder(&self, profile_id: &str, folder: &Path) -> Result<()> {
        self.conn.execute(
            "INSERT OR IGNORE INTO dismissed_folders (profile_id, path) VALUES (?1, ?2)",
            params![profile_id, folder.to_string_lossy()],
        )?;
        tracing::info!(profile_id, folder = %folder.display(), "dismissed library folder");
        Ok(())
    }

    pub fn dismissed_folders(&self, profile_id: &str) -> Result<HashSet<String>> {
        let mut stmt = self
            .conn
            .prepare("SELECT path FROM dismissed_folders WHERE profile_id = ?1")?;
        let paths = stmt
            .query_map(params![profile_id], |row| row.get(0))?
            .collect::<rusqlite::Result<HashSet<String>>>()?;
        tracing::debug!(profile_id, count = paths.len(), "read dismissed library folders");
        Ok(paths)
    }
}
