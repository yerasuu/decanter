//! Library folders a profile keeps out of its Library views. Keyed by the
//! folder's path, as `profile_libraries` and `library_caches` name it, since
//! a folder has no identity of its own; a folder with no row is shown.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use rusqlite::params;

use super::{Db, Error, Result};

impl Db {
    /// The folders `profile_id` hid from its Library views.
    ///
    /// # Errors
    ///
    /// Fails when the database can't be read.
    #[inline]
    pub fn hidden_folders(&self, profile_id: &str) -> Result<HashSet<PathBuf>> {
        let mut stmt = self
            .conn
            .prepare("SELECT folder FROM hidden_folders WHERE profile_id = ?1")?;
        let rows = stmt.query_map(params![profile_id], |row| row.get::<_, String>(0).map(PathBuf::from))?;
        let folders = rows.collect::<rusqlite::Result<HashSet<_>>>().map_err(Error::from)?;
        tracing::debug!(profile_id, count = folders.len(), "listed hidden library folders");
        Ok(folders)
    }

    /// Hides `folder` from `profile_id`'s Library views, or shows it again.
    ///
    /// # Errors
    ///
    /// Fails when the database can't be written.
    #[inline]
    pub fn set_folder_hidden(&self, profile_id: &str, folder: &Path, hidden: bool) -> Result<()> {
        let folder_text = folder.to_string_lossy();
        if hidden {
            self.conn.execute(
                "INSERT OR IGNORE INTO hidden_folders (profile_id, folder) VALUES (?1, ?2)",
                params![profile_id, folder_text],
            )?;
        } else {
            self.conn.execute(
                "DELETE FROM hidden_folders WHERE profile_id = ?1 AND folder = ?2",
                params![profile_id, folder_text],
            )?;
        }
        tracing::info!(profile_id, folder = %folder.display(), hidden, "library folder hidden state changed");
        Ok(())
    }
}
