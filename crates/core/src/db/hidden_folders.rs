//! Library folders a profile keeps out of its Library views. Keyed by the
//! folder's path, as `profile_libraries` and `library_caches` name it, since
//! a folder has no identity of its own; a folder with no row is shown.
//!
//! A hidden folder marks the games it holds as hidden with rows in `hidden` whose `origin` is the
//! folder, so they read like any hidden game; a game the user shows again gets a row in
//! `shown_in_hidden_folders` instead, so the next sync doesn't mark it again.

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

use rusqlite::params;

use super::{Db, Error, Result};
use crate::catalog::Storefront;
use crate::library::InstalledGame;

/// Every id rows of `profile_id` (bound as `?1`) are filed under: its accounts, and the profile
/// itself for external games. Scopes what a folder owns, since two profiles can hide the same one.
const PROFILE_OWNERS: &str = "SELECT id FROM accounts WHERE profile_id = ?1 UNION SELECT ?1";

/// The tables a hidden folder owns rows in, with the column naming the folder.
const FOLDER_ROWS: [(&str, &str); 2] = [("hidden", "origin"), ("shown_in_hidden_folders", "folder")];

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
            let tx = self.conn.unchecked_transaction()?;
            tx.execute(
                "DELETE FROM hidden_folders WHERE profile_id = ?1 AND folder = ?2",
                params![profile_id, folder_text],
            )?;
            for (table, column) in FOLDER_ROWS {
                let dropped = tx.execute(
                    &format!("DELETE FROM {table} WHERE {column} = ?2 AND account_id IN ({PROFILE_OWNERS})"),
                    params![profile_id, folder_text],
                )?;
                tracing::debug!(profile_id, folder = %folder.display(), table, dropped, "dropped the folder's rows");
            }
            tx.commit()?;
        }
        tracing::info!(profile_id, folder = %folder.display(), hidden, "library folder hidden state changed");
        Ok(())
    }

    /// Shows a game that sits in the hidden `folder`, remembering it so the folder's sync leaves it
    /// shown until the folder is shown or the game leaves it.
    ///
    /// # Errors
    ///
    /// Fails when the database can't be written; nothing is changed then.
    #[inline]
    pub fn show_in_hidden_folder(
        &self,
        account_id: &str,
        storefront: Storefront,
        product_id: &str,
        folder: &Path,
    ) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
            "DELETE FROM hidden WHERE account_id = ?1 AND storefront = ?2 AND product_id = ?3",
            params![account_id, storefront as i32, product_id],
        )?;
        tx.execute(
            "INSERT OR REPLACE INTO shown_in_hidden_folders (account_id, storefront, product_id, folder)
             VALUES (?1, ?2, ?3, ?4)",
            params![account_id, storefront as i32, product_id, folder.to_string_lossy()],
        )?;
        tx.commit()?;
        tracing::debug!(account_id, ?storefront, product_id, folder = %folder.display(), "shown despite its hidden folder");
        Ok(())
    }

    /// Brings the marks of `profile_id`'s hidden folders in line with the games each one holds now:
    /// a game that arrived is marked unless the user showed it, a game that left loses the mark and
    /// the exception. A user's own mark is never re-owned. Only the folders in `held` are touched,
    /// so a hidden folder whose disk isn't there keeps its marks until it is scanned again.
    ///
    /// Returns how many rows changed, so the caller re-reads the hidden set only when it moved.
    ///
    /// # Errors
    ///
    /// Fails when the database can't be read or written; nothing is changed then.
    #[inline]
    pub fn sync_folder_marks(&self, profile_id: &str, held: &BTreeMap<PathBuf, Vec<InstalledGame>>) -> Result<usize> {
        let tx = self.conn.unchecked_transaction()?;
        let mut changed = 0_usize;
        // Every folder's stale rows go before any mark is added, so a game moved between two hidden
        // folders ends with the mark of the one it is in now rather than an ignored insert.
        for (folder, games) in held {
            let folder_text = folder.to_string_lossy();
            let holds = |account_id: &str, storefront: i32, product_id: &str| {
                games.iter().any(|game| {
                    game.account_id == account_id && game.storefront as i32 == storefront && game.game_id == product_id
                })
            };
            for (table, column) in FOLDER_ROWS {
                let mut stmt = tx.prepare(&format!(
                    "SELECT account_id, storefront, product_id FROM {table}
                     WHERE {column} = ?2 AND account_id IN ({PROFILE_OWNERS})"
                ))?;
                // Collected before deleting, since SQLite leaves undefined which rows a select
                // still visits once its table changes under it.
                let owned = stmt
                    .query_map(params![profile_id, folder_text], |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, i32>(1)?,
                            row.get::<_, String>(2)?,
                        ))
                    })?
                    .collect::<rusqlite::Result<Vec<_>>>()
                    .map_err(Error::from)?;
                for (account_id, storefront, product_id) in owned {
                    if holds(&account_id, storefront, &product_id) {
                        continue;
                    }
                    changed = changed.saturating_add(tx.execute(
                        &format!(
                            "DELETE FROM {table} WHERE account_id = ?1 AND storefront = ?2 AND product_id = ?3
                             AND {column} = ?4"
                        ),
                        params![account_id, storefront, product_id, folder_text],
                    )?);
                }
            }
        }
        for (folder, games) in held {
            let folder_text = folder.to_string_lossy();
            for game in games {
                changed = changed.saturating_add(tx.execute(
                    "INSERT OR IGNORE INTO hidden (account_id, storefront, product_id, origin)
                     SELECT ?1, ?2, ?3, ?4 WHERE NOT EXISTS (
                         SELECT 1 FROM shown_in_hidden_folders
                         WHERE account_id = ?1 AND storefront = ?2 AND product_id = ?3 AND folder = ?4)",
                    params![game.account_id, game.storefront as i32, game.game_id, folder_text],
                )?);
            }
        }
        tx.commit()?;
        tracing::debug!(profile_id, folders = held.len(), changed, "synced hidden folder marks");
        Ok(changed)
    }
}
