//! Profiles — the identity someone picks and switches between, in the
//! navbar's switcher.
//!
//! A profile is deliberately thin — an id and a name — because it's only
//! a grouping: what it groups is the storefront accounts in
//! [`crate::accounts`], and it's those accounts (not the profile) that
//! the library cache, achievements, playtime, installs and the keyring
//! entries in [`crate::credentials`] are actually keyed by. Switching the
//! active profile swaps out the whole set of accounts in play at once.

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

use rusqlite::types::{FromSql, FromSqlError, FromSqlResult, ToSql, ToSqlOutput, ValueRef};
use rusqlite::{OptionalExtension as _, params};

use crate::db::{Db, Error, Result};

/// What a profile plays, which decides the sections it shows.
///
/// Fixed at creation: a PS profile has no storefront accounts and a PC
/// profile has no PS game folders, so switching one over would strand
/// what it holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProfileKind {
    /// GOG accounts, the Proton library and everything built around them.
    Pc,
    /// PS4/PS5 dumps run through the native ports.
    Ps,
}

impl ProfileKind {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Pc => "pc",
            Self::Ps => "ps",
        }
    }
}

impl ToSql for ProfileKind {
    #[inline]
    fn to_sql(&self) -> rusqlite::Result<ToSqlOutput<'_>> {
        Ok(ToSqlOutput::from(self.as_str()))
    }
}

impl FromSql for ProfileKind {
    /// An unknown kind is an error rather than a fallback to `Pc`: it means
    /// a newer decanter wrote this row, and treating its profile as PC would
    /// show it sections built for accounts it doesn't have.
    #[inline]
    fn column_result(value: ValueRef<'_>) -> FromSqlResult<Self> {
        match value.as_str() {
            Ok("pc") => Ok(Self::Pc),
            Ok("ps") => Ok(Self::Ps),
            Ok(other) => Err(FromSqlError::Other(format!("unknown profile kind {other:?}").into())),
            Err(err) => Err(err),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Profile {
    /// A string, not a `Uuid`: stored as SQLite `TEXT` and every cache row
    /// is already keyed by that exact text, so it's carried through as-is
    /// rather than parsed and re-serialized on each read.
    pub id: String,
    pub name: String,
    /// Where this profile's games, prefixes and manifests live. `None`
    /// means `decanter_core::Library::default_path()` — the shared path
    /// every profile used before a per-profile location existed, and still
    /// what a profile that's never chosen one of its own resolves to.
    pub library_path: Option<String>,
    pub kind: ProfileKind,
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
            .query_row(
                "SELECT id, name, library_path, kind FROM profiles WHERE id = ?1",
                params![id],
                |row| {
                    Ok(Profile {
                        id: match row.get(0) {
                            Ok(value) => value,
                            Err(err) => return Err(err),
                        },
                        name: match row.get(1) {
                            Ok(value) => value,
                            Err(err) => return Err(err),
                        },
                        library_path: match row.get(2) {
                            Ok(value) => value,
                            Err(err) => return Err(err),
                        },
                        kind: match row.get(3) {
                            Ok(value) => value,
                            Err(err) => return Err(err),
                        },
                    })
                },
            )
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
        // Only the folder list: the saves under the profile's PS data
        // directory stay on disk, the same as an account's installs do.
        match self
            .conn
            .execute("DELETE FROM ps_scan_dirs WHERE profile_id = ?1", params![profile_id])
        {
            Ok(_) => {}
            Err(err) => return Err(err.into()),
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
            .prepare("SELECT id, name, library_path, kind FROM profiles ORDER BY created_at ASC, name ASC")
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
                library_path: match row.get(2) {
                    Ok(value) => value,
                    Err(err) => return Err(err),
                },
                kind: match row.get(3) {
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
    /// what the user is used to seeing. `kind` only applies to a new
    /// profile: a rename keeps the kind it was created with.
    #[inline]
    pub fn upsert_profile(&self, id: &str, name: &str, kind: ProfileKind, created_at: i64) -> Result<Profile> {
        match self.conn.execute(
            "INSERT INTO profiles (id, name, kind, created_at) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(id) DO UPDATE SET name = excluded.name",
            params![id, name, kind, created_at],
        ) {
            Ok(_) => {}
            Err(err) => return Err(err.into()),
        }
        // A milestone worth seeing at default level, same tier as
        // `Db::upsert_account`'s own info!.
        tracing::info!(profile_id = id, name, "upserted profile");
        // Read back rather than assuming: a rename of a profile that
        // already picked its own library location must not report that
        // location as unset, nor its kind as the one passed in, just
        // because this call never touches those columns.
        let (library_path, stored_kind) = self
            .conn
            .query_row(
                "SELECT library_path, kind FROM profiles WHERE id = ?1",
                params![id],
                |row| match (row.get(0), row.get(1)) {
                    (Ok(library_path), Ok(stored_kind)) => Ok((library_path, stored_kind)),
                    (Err(err), _) | (_, Err(err)) => Err(err),
                },
            )
            .map_err(Error::from)?;
        Ok(Profile {
            id: id.to_owned(),
            name: name.to_owned(),
            library_path,
            kind: stored_kind,
        })
    }

    /// Where `profile_id`'s library lives, if it's ever chosen one of its
    /// own — `None` means `decanter_core::Library::default_path()`.
    #[inline]
    pub fn profile_library_path(&self, profile_id: &str) -> Result<Option<String>> {
        let path = self
            .conn
            .query_row(
                "SELECT library_path FROM profiles WHERE id = ?1",
                params![profile_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(Error::from)?
            .flatten();
        tracing::debug!(profile_id, found = path.is_some(), "read profile library path");
        Ok(path)
    }

    /// Sets (or clears, with `None`) the profile's chosen library location.
    #[inline]
    pub fn set_profile_library_path(&self, profile_id: &str, path: Option<&str>) -> Result<()> {
        match self.conn.execute(
            "UPDATE profiles SET library_path = ?2 WHERE id = ?1",
            params![profile_id, path],
        ) {
            Ok(_) => {}
            Err(err) => return Err(err.into()),
        }
        tracing::info!(profile_id, path, "set profile library path");
        Ok(())
    }
}

impl Db {
    /// A PS profile's game folders, in the order they were added.
    #[inline]
    pub fn ps_scan_dirs(&self, profile_id: &str) -> Result<Vec<String>> {
        let mut statement = match self
            .conn
            .prepare("SELECT path FROM ps_scan_dirs WHERE profile_id = ?1 ORDER BY rowid")
        {
            Ok(statement) => statement,
            Err(err) => return Err(err.into()),
        };
        let rows = match statement.query_map(params![profile_id], |row| row.get(0)) {
            Ok(rows) => rows,
            Err(err) => return Err(err.into()),
        };
        Ok(rows.flatten().collect())
    }

    /// Adding a folder that is already listed is a no-op.
    #[inline]
    pub fn add_ps_scan_dir(&self, profile_id: &str, path: &str) -> Result<()> {
        match self.conn.execute(
            "INSERT OR IGNORE INTO ps_scan_dirs (profile_id, path) VALUES (?1, ?2)",
            params![profile_id, path],
        ) {
            Ok(_) => {}
            Err(err) => return Err(err.into()),
        }
        tracing::info!(profile_id, path, "added PS game folder");
        Ok(())
    }

    #[inline]
    pub fn remove_ps_scan_dir(&self, profile_id: &str, path: &str) -> Result<()> {
        match self.conn.execute(
            "DELETE FROM ps_scan_dirs WHERE profile_id = ?1 AND path = ?2",
            params![profile_id, path],
        ) {
            Ok(_) => {}
            Err(err) => return Err(err.into()),
        }
        tracing::info!(profile_id, path, "removed PS game folder");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rename_keeps_the_kind_a_profile_was_created_with() {
        let db = Db::open(":memory:").unwrap();
        db.upsert_profile("a", "Consola", ProfileKind::Ps, 1).unwrap();
        let renamed = db.upsert_profile("a", "Sala", ProfileKind::Pc, 2).unwrap();
        assert_eq!((renamed.name.as_str(), renamed.kind), ("Sala", ProfileKind::Ps));
        let listed = db.list_profiles().unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].kind, ProfileKind::Ps);
    }

    #[test]
    fn scan_dirs_belong_to_their_profile_and_go_with_it() {
        let db = Db::open(":memory:").unwrap();
        db.upsert_profile("a", "Consola", ProfileKind::Ps, 1).unwrap();
        db.upsert_profile("b", "Otra", ProfileKind::Ps, 2).unwrap();
        db.add_ps_scan_dir("a", "/games/ps4").unwrap();
        db.add_ps_scan_dir("a", "/games/ps5").unwrap();
        db.add_ps_scan_dir("a", "/games/ps4").unwrap();
        db.add_ps_scan_dir("b", "/other").unwrap();
        assert_eq!(db.ps_scan_dirs("a").unwrap(), ["/games/ps4", "/games/ps5"]);
        db.remove_ps_scan_dir("a", "/games/ps4").unwrap();
        assert_eq!(db.ps_scan_dirs("a").unwrap(), ["/games/ps5"]);
        db.delete_profile("a").unwrap();
        assert!(db.ps_scan_dirs("a").unwrap().is_empty());
        assert_eq!(db.ps_scan_dirs("b").unwrap(), ["/other"]);
    }

    #[test]
    fn profiles_from_before_kinds_open_as_pc() {
        let path = std::env::temp_dir().join(format!("decanter-profiles-{}.db", std::process::id()));
        {
            let conn = rusqlite::Connection::open(&path).unwrap();
            conn.execute_batch(
                "CREATE TABLE profiles (id TEXT PRIMARY KEY, name TEXT NOT NULL, created_at INTEGER NOT NULL);
                 INSERT INTO profiles VALUES ('old', 'Antiguo', 1);",
            )
            .unwrap();
        }
        let db = Db::open(&path).unwrap();
        let profiles = db.list_profiles().unwrap();
        drop(db);
        std::fs::remove_file(&path).unwrap();
        assert_eq!(profiles.len(), 1);
        assert_eq!(profiles[0].kind, ProfileKind::Pc);
    }
}
