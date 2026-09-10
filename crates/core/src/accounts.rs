//! Accounts — the storefront logins grouped under a [`crate::profiles::Profile`].
//!
//! A profile can hold several accounts, even several of the same
//! storefront (two GOG logins, say) — [`Account::id`] is the storefront's
//! own account id, so `(id, storefront)` is what tells two accounts apart,
//! not which profile they belong to. Library, achievements, playtime and
//! the keyring entries in [`crate::credentials`] are keyed by account, not
//! by profile — see [`crate::db`] for why.

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

use rusqlite::params;

use crate::catalog::Storefront;
use crate::db::{Db, Result};

#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Account {
    /// The storefront's own id for this account (for GOG, its numeric
    /// `account_id` as a decimal string) — not one decanter makes up, so a
    /// login always lands on the same row.
    pub id: String,
    pub name: String,
    /// Which profile this account is grouped under.
    pub profile_id: String,
    pub storefront: Storefront,
}

impl Db {
    /// Every account grouped under `profile_id`, oldest first — the order
    /// the Library merges them in and Ajustes lists them in.
    #[inline]
    pub fn accounts_in_profile(&self, profile_id: &str) -> Result<Vec<Account>> {
        let mut statement = match self.conn.prepare(
            "SELECT id, storefront, profile_id, name FROM accounts WHERE profile_id = ?1 ORDER BY created_at ASC",
        ) {
            Ok(statement) => statement,
            Err(err) => return Err(err.into()),
        };
        let rows = match statement.query_map(params![profile_id], |row| {
            Ok(Account {
                id: match row.get(0) {
                    Ok(value) => value,
                    Err(err) => return Err(err),
                },
                storefront: storefront_from_row(match row.get(1) {
                    Ok(value) => value,
                    Err(err) => return Err(err),
                }),
                profile_id: match row.get(2) {
                    Ok(value) => value,
                    Err(err) => return Err(err),
                },
                name: match row.get(3) {
                    Ok(value) => value,
                    Err(err) => return Err(err),
                },
            })
        }) {
            Ok(rows) => rows,
            Err(err) => return Err(err.into()),
        };
        let accounts: Vec<_> = rows.flatten().collect();
        tracing::debug!(profile_id, account_count = accounts.len(), "listed accounts in profile");
        Ok(accounts)
    }

    /// Forgets the account itself (a logout). Its cached library,
    /// achievements and installs are left in place, keyed by the same id —
    /// re-adding the account brings them all back rather than starting the
    /// user over, same rationale as [`Db::delete_profile`].
    #[inline]
    pub fn forget_account(&self, id: &str, storefront: Storefront) -> Result<()> {
        match self.conn.execute(
            "DELETE FROM accounts WHERE id = ?1 AND storefront = ?2",
            params![id, storefront as i32],
        ) {
            Ok(_) => {}
            Err(err) => return Err(err.into()),
        }
        // A logout is a milestone worth seeing at default level, unlike the
        // routine per-query reads elsewhere in this crate.
        tracing::info!(account_id = id, ?storefront, "forgot account");
        Ok(())
    }

    /// Adds an account to `profile_id`, or moves/renames the one that
    /// already has this `(id, storefront)` — a re-login always lands on
    /// the same row, even if it's since been moved to a different profile.
    #[inline]
    pub fn upsert_account(
        &self,
        id: &str,
        storefront: Storefront,
        profile_id: &str,
        name: &str,
        created_at: i64,
    ) -> Result<Account> {
        match self.conn.execute(
            "INSERT INTO accounts (id, storefront, profile_id, name, created_at) VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(id, storefront) DO UPDATE SET name = excluded.name, profile_id = excluded.profile_id",
            params![id, storefront as i32, profile_id, name, created_at],
        ) {
            Ok(_) => {}
            Err(err) => return Err(err.into()),
        }
        // A milestone worth seeing at default level: a login landed (or
        // moved profile), unlike the routine per-query reads elsewhere.
        tracing::info!(account_id = id, ?storefront, profile_id, "upserted account");
        Ok(Account {
            id: id.to_owned(),
            storefront,
            profile_id: profile_id.to_owned(),
            name: name.to_owned(),
        })
    }
}

/// Maps an out-of-range storefront discriminant to `Unspecified` rather
/// than failing the whole row — same defensive read as [`crate::db`]'s own
/// `games_cache` row mapping.
fn storefront_from_row(raw: i32) -> Storefront {
    Storefront::try_from(raw).unwrap_or_else(|_| {
        tracing::warn!(raw, "unrecognized storefront in accounts, defaulting to Unspecified");
        Storefront::Unspecified
    })
}
