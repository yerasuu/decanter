//! User-made shelves ("collections") in Biblioteca, plus per-profile hidden
//! games. Kept apart from [`crate::db`]'s own concerns for the same reason
//! [`crate::profiles`] is — see `lib.rs`'s doc comment.
//!
//! A collection is either explicit (games added one at a time, tracked in
//! `collection_members`) or dynamic (its `filter` column holds an encoded
//! saved filter, evaluated live by the UI against the current library —
//! `collection_members` rows for a dynamic collection are never read).
//! Ordering is a plain per-profile position, shifted by [`Db::move_collection`].

use std::collections::{HashMap, HashSet};

use rusqlite::params;
use uuid::Uuid;

use crate::catalog::Storefront;
use crate::db::{Db, Error, Result};

#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Collection {
    pub id: String,
    pub name: String,
    /// `Some` makes this a dynamic collection — an encoded saved filter,
    /// evaluated live rather than backed by [`Db::collection_members`].
    pub filter: Option<String>,
}

impl Db {
    /// One profile's collections, in display order.
    pub fn list_collections(&self, profile_id: &str) -> Result<Vec<Collection>> {
        let mut stmt = self
            .conn
            .prepare("SELECT id, name, filter FROM collections WHERE profile_id = ?1 ORDER BY position ASC")?;
        let rows = stmt.query_map(params![profile_id], |row| {
            Ok(Collection {
                id: row.get(0)?,
                name: row.get(1)?,
                filter: row.get(2)?,
            })
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>().map_err(Error::from)
    }

    /// Explicit membership for every one of a profile's collections, by
    /// collection id — meaningless for a dynamic collection (see the
    /// module doc), but harmless to read since nothing is ever written
    /// there for one.
    pub fn collection_members(
        &self,
        profile_id: &str,
        storefront: Storefront,
    ) -> Result<HashMap<String, HashSet<String>>> {
        let mut stmt = self.conn.prepare(
            "SELECT collection_members.collection_id, collection_members.product_id
             FROM collection_members
             JOIN collections ON collections.id = collection_members.collection_id
             WHERE collections.profile_id = ?1 AND collection_members.storefront = ?2",
        )?;
        let rows = stmt.query_map(params![profile_id, storefront as i32], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        let mut members: HashMap<String, HashSet<String>> = HashMap::new();
        for row in rows {
            let (collection_id, product_id) = row?;
            members.entry(collection_id).or_default().insert(product_id);
        }
        Ok(members)
    }

    /// Adds a new, empty, non-dynamic collection at the end of the
    /// profile's order.
    pub fn create_collection(&self, profile_id: &str, name: &str) -> Result<Collection> {
        let id = Uuid::new_v4().to_string();
        let next_position: i64 = self.conn.query_row(
            "SELECT COALESCE(MAX(position), -1) + 1 FROM collections WHERE profile_id = ?1",
            params![profile_id],
            |row| row.get(0),
        )?;
        self.conn.execute(
            "INSERT INTO collections (id, profile_id, name, filter, position) VALUES (?1, ?2, ?3, NULL, ?4)",
            params![id, profile_id, name, next_position],
        )?;
        Ok(Collection {
            id,
            name: name.to_owned(),
            filter: None,
        })
    }

    pub fn rename_collection(&self, collection_id: &str, name: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE collections SET name = ?1 WHERE id = ?2",
            params![name, collection_id],
        )?;
        Ok(())
    }

    /// Sets or clears a collection's saved filter — `Some` makes (or keeps)
    /// it dynamic, `None` makes it explicit again.
    pub fn set_collection_filter(&self, collection_id: &str, filter: Option<&str>) -> Result<()> {
        self.conn.execute(
            "UPDATE collections SET filter = ?1 WHERE id = ?2",
            params![filter, collection_id],
        )?;
        Ok(())
    }

    /// Removes a collection and every explicit membership row it had.
    pub fn delete_collection(&self, collection_id: &str) -> Result<()> {
        self.conn.execute(
            "DELETE FROM collection_members WHERE collection_id = ?1",
            params![collection_id],
        )?;
        self.conn
            .execute("DELETE FROM collections WHERE id = ?1", params![collection_id])?;
        Ok(())
    }

    /// Adds or drops one game from an explicit collection. Writing to a
    /// dynamic collection's membership isn't rejected here — its rows are
    /// simply never read while `filter` is set (see the module doc); the
    /// UI itself disables the action for one.
    pub fn set_collection_member(
        &self,
        collection_id: &str,
        storefront: Storefront,
        product_id: &str,
        member: bool,
    ) -> Result<()> {
        if member {
            self.conn.execute(
                "INSERT OR IGNORE INTO collection_members (collection_id, storefront, product_id) VALUES (?1, ?2, ?3)",
                params![collection_id, storefront as i32, product_id],
            )?;
        } else {
            self.conn.execute(
                "DELETE FROM collection_members WHERE collection_id = ?1 AND storefront = ?2 AND product_id = ?3",
                params![collection_id, storefront as i32, product_id],
            )?;
        }
        Ok(())
    }

    /// Shifts one profile's collection `delta` slots in its display order
    /// — negative earlier, positive later. Clamped to the ends rather than
    /// erroring on a shift past either edge.
    pub fn move_collection(&self, profile_id: &str, collection_id: &str, delta: i64) -> Result<()> {
        let mut ids: Vec<String> = {
            let mut stmt = self
                .conn
                .prepare("SELECT id FROM collections WHERE profile_id = ?1 ORDER BY position ASC")?;
            let rows = stmt.query_map(params![profile_id], |row| row.get(0))?;
            rows.collect::<rusqlite::Result<_>>()?
        };
        let Some(from) = ids.iter().position(|id| id == collection_id) else {
            return Ok(());
        };
        #[expect(
            clippy::as_conversions,
            reason = "a profile's collection count never approaches i64::MAX"
        )]
        let to = (from as i64 + delta).clamp(0, ids.len() as i64 - 1) as usize;
        if to == from {
            return Ok(());
        }
        let moved = ids.remove(from);
        ids.insert(to, moved);

        let tx = self.conn.unchecked_transaction()?;
        for (position, id) in ids.iter().enumerate() {
            #[expect(
                clippy::as_conversions,
                reason = "a profile's collection count never approaches i64::MAX"
            )]
            let position = position as i64;
            tx.execute(
                "UPDATE collections SET position = ?1 WHERE id = ?2",
                params![position, id],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Product ids hidden from a profile's library view entirely.
    pub fn hidden_ids(&self, profile_id: &str, storefront: Storefront) -> Result<HashSet<String>> {
        let mut stmt = self
            .conn
            .prepare("SELECT product_id FROM hidden WHERE profile_id = ?1 AND storefront = ?2")?;
        let rows = stmt.query_map(params![profile_id, storefront as i32], |row| row.get(0))?;
        rows.collect::<rusqlite::Result<_>>().map_err(Error::from)
    }

    pub fn set_hidden(&self, profile_id: &str, storefront: Storefront, product_id: &str, hidden: bool) -> Result<()> {
        if hidden {
            self.conn.execute(
                "INSERT OR IGNORE INTO hidden (profile_id, storefront, product_id) VALUES (?1, ?2, ?3)",
                params![profile_id, storefront as i32, product_id],
            )?;
        } else {
            self.conn.execute(
                "DELETE FROM hidden WHERE profile_id = ?1 AND storefront = ?2 AND product_id = ?3",
                params![profile_id, storefront as i32, product_id],
            )?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
