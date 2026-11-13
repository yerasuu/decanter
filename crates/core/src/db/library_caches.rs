//! Where each library folder keeps its games' shader caches, when the user
//! pointed it somewhere other than [`crate::default_cache_root`]: a folder
//! on a slow disk can keep them on a fast one. Keyed by the folder's path,
//! as `profile_libraries` and `profiles.library_path` name it; a folder
//! with no row uses its default.

use std::path::Path;

use rusqlite::{OptionalExtension as _, params};

use super::{Db, Error, Result};

impl Db {
    /// The cache location chosen for `folder`, `None` when it keeps the default.
    pub fn library_cache(&self, folder: &Path) -> Result<Option<String>> {
        self.conn
            .query_row(
                "SELECT cache_path FROM library_caches WHERE folder = ?1",
                params![folder.to_string_lossy()],
                |row| row.get(0),
            )
            .optional()
            .map_err(Error::from)
    }

    /// Points `folder`'s caches at `cache_path`, or back at the default with `None`.
    pub fn set_library_cache(&self, folder: &Path, cache_path: Option<&Path>) -> Result<()> {
        let folder_text = folder.to_string_lossy();
        match cache_path {
            Some(path) => self.conn.execute(
                "INSERT INTO library_caches (folder, cache_path) VALUES (?1, ?2)
                 ON CONFLICT(folder) DO UPDATE SET cache_path = excluded.cache_path",
                params![folder_text, path.to_string_lossy()],
            )?,
            None => self
                .conn
                .execute("DELETE FROM library_caches WHERE folder = ?1", params![folder_text])?,
        };
        tracing::info!(folder = %folder.display(), cache_path = ?cache_path, "library cache location changed");
        Ok(())
    }
}
