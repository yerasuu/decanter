//! Profiles — the identity someone picks and switches between, in the
//! navbar's switcher.
//!
//! A profile is deliberately thin — an id and a name — because it's only
//! a grouping: what it groups is the storefront accounts in
//! [`crate::accounts`], and it's those accounts (not the profile) that
//! the library cache, achievements, playtime, installs and the keyring
//! entries in [`crate::credentials`] are actually keyed by. Switching the
//! active profile swaps out the whole set of accounts in play at once.

// Only fires on some compilation targets (lib vs tests), so `expect` can't
// be used here without an "unfulfilled expectation" warning on the other.
#![allow(
    clippy::self_named_module_files,
    reason = "convention here is `<name>.rs` plus a sibling `<name>/tests.rs`, see CLAUDE.md"
)]
#![expect(
    clippy::implicit_return,
    reason = "directly contradicts needless_return from clippy::all, also enabled"
)]
#![expect(
    clippy::large_stack_frames,
    reason = "every rusqlite row-mapping closure below already costs more than the 512-byte threshold"
)]
#![expect(
    clippy::missing_errors_doc,
    reason = "every fallible method here fails for the same reason (the underlying SQL query failed); \
              a # Errors section repeating that is exactly the restates-the-signature filler \
              this codebase's own humanization scanner rejects"
)]
#![expect(
    clippy::multiple_inherent_impl,
    reason = "Db's API is deliberately split by concern across modules, see lib.rs's doc comment"
)]

use rusqlite::{OptionalExtension as _, params};

use crate::db::{Db, Error, Result};

#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Profile {
    /// A string, not a `Uuid`: profiles imported from bottles-next keep
    /// their original ids, and every cache row already written is keyed by
    /// that exact text.
    pub id: String,
    pub name: String,
}

impl Db {
    /// The profile in play. `None` on a fresh install, and also when the
    /// stored choice points at a profile that has since been deleted.
    #[inline]
    pub fn active_profile(&self) -> Result<Option<Profile>> {
        let Some(id) = (match self.setting("active_profile_id") {
            Ok(id) => id,
            Err(err) => return Err(err),
        }) else {
            return Ok(None);
        };
        self.conn
            .query_row("SELECT id, name FROM profiles WHERE id = ?1", params![id], |row| {
                Ok(Profile {
                    id: match row.get(0) {
                        Ok(value) => value,
                        Err(err) => return Err(err),
                    },
                    name: match row.get(1) {
                        Ok(value) => value,
                        Err(err) => return Err(err),
                    },
                })
            })
            .optional()
            .map_err(Error::from)
    }

    /// Forgets the profile itself. Its cached library, achievements and
    /// installs are left in place: they are keyed by the same id, so
    /// re-adding the profile brings them all back rather than starting the
    /// user over.
    #[inline]
    pub fn delete_profile(&self, profile_id: &str) -> Result<()> {
        match self
            .conn
            .execute("DELETE FROM profiles WHERE id = ?1", params![profile_id])
        {
            Ok(_) => {}
            Err(err) => return Err(err.into()),
        }
        Ok(())
    }

    #[inline]
    pub fn list_profiles(&self) -> Result<Vec<Profile>> {
        let mut statement = match self
            .conn
            .prepare("SELECT id, name FROM profiles ORDER BY created_at ASC, name ASC")
        {
            Ok(statement) => statement,
            Err(err) => return Err(err.into()),
        };
        let rows = match statement.query_map([], |row| {
            Ok(Profile {
                id: match row.get(0) {
                    Ok(value) => value,
                    Err(err) => return Err(err),
                },
                name: match row.get(1) {
                    Ok(value) => value,
                    Err(err) => return Err(err),
                },
            })
        }) {
            Ok(rows) => rows,
            Err(err) => return Err(err.into()),
        };
        Ok(rows.flatten().collect())
    }

    #[inline]
    pub fn set_active_profile(&self, profile_id: &str) -> Result<()> {
        self.set_setting("active_profile_id", profile_id)
    }

    /// Adds a profile, or renames the one that already has this id.
    /// `created_at` only orders the listing, so re-adding never reshuffles
    /// what the user is used to seeing.
    #[inline]
    pub fn upsert_profile(&self, id: &str, name: &str, created_at: i64) -> Result<Profile> {
        match self.conn.execute(
            "INSERT INTO profiles (id, name, created_at) VALUES (?1, ?2, ?3)
             ON CONFLICT(id) DO UPDATE SET name = excluded.name",
            params![id, name, created_at],
        ) {
            Ok(_) => {}
            Err(err) => return Err(err.into()),
        }
        Ok(Profile {
            id: id.to_owned(),
            name: name.to_owned(),
        })
    }
}

#[cfg(test)]
mod tests;
