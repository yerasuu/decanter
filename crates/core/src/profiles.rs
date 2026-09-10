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
    /// A string, not a `Uuid`: stored as SQLite `TEXT` and every cache row
    /// is already keyed by that exact text, so it's carried through as-is
    /// rather than parsed and re-serialized on each read.
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
            tracing::debug!("no active profile set");
            return Ok(None);
        };
        let profile = self
            .conn
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
            .map_err(Error::from)?;
        if profile.is_none() {
            // The stored active_profile_id points at a profile that's since
            // been deleted — recoverable (caller just sees no active
            // profile), but worth flagging since it's stale settings state.
            tracing::warn!(profile_id = id, "active profile id set but no matching profile exists");
        }
        tracing::debug!(profile_id = id, found = profile.is_some(), "read active profile");
        Ok(profile)
    }

    /// Forgets the profile itself, and every account it grouped — unlike
    /// forgetting one account, a profile can't be left holding accounts
    /// with no profile of their own, so this cascades. Each account's
    /// cached library, achievements and installs are left in place, same
    /// as [`Db::forget_account`] alone: re-adding the profile and its
    /// accounts brings them all back rather than starting the user over.
    #[inline]
    pub fn delete_profile(&self, profile_id: &str) -> Result<()> {
        let accounts = match self.accounts_in_profile(profile_id) {
            Ok(accounts) => accounts,
            Err(err) => return Err(err),
        };
        for account in accounts {
            if let Err(err) = self.forget_account(&account.id, account.storefront) {
                return Err(err);
            }
        }
        match self
            .conn
            .execute("DELETE FROM profiles WHERE id = ?1", params![profile_id])
        {
            Ok(_) => {}
            Err(err) => return Err(err.into()),
        }
        // A milestone worth seeing at default level, same tier as
        // `Db::forget_account`'s own info! for one account alone.
        tracing::info!(profile_id, "deleted profile");
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
        let profiles: Vec<_> = rows.flatten().collect();
        tracing::debug!(profile_count = profiles.len(), "listed profiles");
        Ok(profiles)
    }

    #[inline]
    pub fn set_active_profile(&self, profile_id: &str) -> Result<()> {
        // A profile switch is a milestone worth seeing at default level:
        // it swaps out the whole set of accounts in play at once.
        tracing::info!(profile_id, "switched active profile");
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
        // A milestone worth seeing at default level, same tier as
        // `Db::upsert_account`'s own info!.
        tracing::info!(profile_id = id, name, "upserted profile");
        Ok(Profile {
            id: id.to_owned(),
            name: name.to_owned(),
        })
    }
}

#[cfg(test)]
mod tests;
