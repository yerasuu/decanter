//! The measured size of each PS dump, which has no manifest to keep it in
//! the way a PC game does. Kept with the [`crate::install_fingerprint`] it
//! was measured at, so a dump is walked again only once that changes;
//! keyed by the dump's folder.

use std::path::Path;

use rusqlite::{OptionalExtension as _, params};

use super::{Db, Error, Result};

impl Db {
    /// `(size, fingerprint)` as last measured.
    pub fn dump_size(&self, dir: &Path) -> Result<Option<(u64, String)>> {
        self.conn
            .query_row(
                "SELECT size, fingerprint FROM dump_sizes WHERE dir = ?1",
                params![dir.to_string_lossy()],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()
            .map(|found| found.map(|(size, fingerprint)| (u64::try_from(size).unwrap_or(0), fingerprint)))
            .map_err(Error::from)
    }

    pub fn set_dump_size(&self, dir: &Path, size: u64, fingerprint: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO dump_sizes (dir, size, fingerprint) VALUES (?1, ?2, ?3)
             ON CONFLICT(dir) DO UPDATE SET size = excluded.size, fingerprint = excluded.fingerprint",
            params![
                dir.to_string_lossy(),
                i64::try_from(size).unwrap_or(i64::MAX),
                fingerprint
            ],
        )?;
        Ok(())
    }

    /// Follows a dump moved whole from `old` to `new`: same files, same size.
    pub fn move_dump_size(&self, old: &Path, new: &Path) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO dump_sizes (dir, size, fingerprint)
             SELECT ?2, size, fingerprint FROM dump_sizes WHERE dir = ?1",
            params![old.to_string_lossy(), new.to_string_lossy()],
        )?;
        self.conn
            .execute("DELETE FROM dump_sizes WHERE dir = ?1", params![old.to_string_lossy()])?;
        Ok(())
    }
}
