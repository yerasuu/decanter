//! Self-tracked play sessions, per game.

use rusqlite::{OptionalExtension as _, params};

use super::{Db, Error, Result};
use crate::catalog::Storefront;

/// Self-tracked: nothing upstream reports play sessions, so
/// `seconds_total` accumulates whatever `decanter` itself measured between a
/// launch it made and the process exiting.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Playtime {
    pub seconds_total: i64,
    pub last_played_at: Option<i64>,
}

impl Db {
    pub fn get_playtime(&self, account_id: &str, storefront: Storefront, product_id: &str) -> Result<Playtime> {
        let playtime = self
            .conn
            .query_row(
                "SELECT seconds_total, last_played_at FROM playtime
                 WHERE account_id = ?1 AND storefront = ?2 AND product_id = ?3",
                params![account_id, storefront as i32, product_id],
                |row| {
                    Ok(Playtime {
                        seconds_total: row.get(0)?,
                        last_played_at: row.get(1)?,
                    })
                },
            )
            .optional()
            .map(Option::unwrap_or_default)
            .map_err(Error::from)?;
        tracing::debug!(
            account_id,
            ?storefront,
            product_id,
            seconds_total = playtime.seconds_total,
            "read playtime"
        );
        Ok(playtime)
    }

    /// Adds one session's worth of seconds and stamps `last_played_at` to
    /// now. Called once, when the launched process exits.
    pub fn add_playtime(&self, account_id: &str, storefront: Storefront, product_id: &str, seconds: i64) -> Result<()> {
        self.conn.execute(
            "INSERT INTO playtime (account_id, storefront, product_id, seconds_total, last_played_at)
             VALUES (?1, ?2, ?3, ?4, strftime('%s','now'))
             ON CONFLICT(account_id, storefront, product_id) DO UPDATE SET
                seconds_total = seconds_total + excluded.seconds_total,
                last_played_at = excluded.last_played_at",
            params![account_id, storefront as i32, product_id, seconds],
        )?;
        // A milestone worth seeing at default level: one play session just
        // ended and got credited, unlike the routine per-query reads above.
        tracing::info!(account_id, ?storefront, product_id, seconds, "recorded playtime");
        Ok(())
    }

    /// Every product id's playtime for an account, in one query — for the
    /// sidebar, which needs all of them at once rather than one round trip
    /// per game.
    pub fn all_playtime(
        &self,
        account_id: &str,
        storefront: Storefront,
    ) -> Result<std::collections::HashMap<String, Playtime>> {
        let mut stmt = self.conn.prepare(
            "SELECT product_id, seconds_total, last_played_at FROM playtime WHERE account_id = ?1 AND storefront = ?2",
        )?;
        let rows = stmt.query_map(params![account_id, storefront as i32], |row| {
            Ok((
                row.get::<_, String>(0)?,
                Playtime {
                    seconds_total: row.get(1)?,
                    last_played_at: row.get(2)?,
                },
            ))
        })?;
        let playtimes = rows
            .collect::<rusqlite::Result<std::collections::HashMap<_, _>>>()
            .map_err(Error::from)?;
        tracing::debug!(account_id, ?storefront, count = playtimes.len(), "read all playtime");
        Ok(playtimes)
    }
}
