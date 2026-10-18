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

    /// Follows a library folder moved from `old` to `new`: its own row, and
    /// any folder's cache that lived inside it, since that moved with it.
    pub fn rebase_library_caches(&self, old: &Path, new: &Path) -> Result<()> {
        let mut stmt = self.conn.prepare("SELECT folder, cache_path FROM library_caches")?;
        let rows = stmt
            .query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let rebase = |path: &str| {
            Path::new(path).strip_prefix(old).map_or_else(
                |_| Path::new(path).to_path_buf(),
                |relative| {
                    if relative.as_os_str().is_empty() {
                        new.to_path_buf()
                    } else {
                        new.join(relative)
                    }
                },
            )
        };
        for (folder, cache_path) in &rows {
            let (moved_folder, moved_cache) = (rebase(folder), rebase(cache_path));
            if moved_folder.as_path() == Path::new(folder) && moved_cache.as_path() == Path::new(cache_path) {
                continue;
            }
            self.conn.execute(
                "UPDATE library_caches SET folder = ?2, cache_path = ?3 WHERE folder = ?1",
                params![folder, moved_folder.to_string_lossy(), moved_cache.to_string_lossy()],
            )?;
            tracing::info!(folder, to = %moved_folder.display(), cache_path = %moved_cache.display(), "library cache location followed a moved library");
        }
        Ok(())
    }
}
