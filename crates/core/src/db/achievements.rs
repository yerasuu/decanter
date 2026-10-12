//! Per-game achievement cache, and the hook failures recorded alongside it.

use rusqlite::{OptionalExtension as _, params};

use super::{Db, Error, Result};
use crate::catalog::Storefront;

#[derive(Debug, Clone, PartialEq)]
pub struct CachedAchievement {
    pub achievement_key: String,
    pub name: String,
    pub description: Option<String>,
    pub icon_unlocked_url: Option<String>,
    pub icon_locked_url: Option<String>,
    pub unlocked: bool,
    pub unlocked_at: Option<String>,
}

/// One `decanter-hook` failure to install its `SetAchievement` patch,
/// carried home from that game's own JSONL log -- see that crate's
/// `log::report_diagnostic` for what `stage` and `detail` actually hold.
/// Kept distinct from a plain trace line because these are exactly the
/// cases worth going back to adapt `abi_scan` for.
#[derive(Debug, Clone, PartialEq)]
pub struct HookDiagnostic {
    pub recorded_at: i64,
    pub stage: String,
    pub detail: String,
}

impl Db {
    /// `(unlocked, total)` achievement counts cached for one game.
    pub fn achievement_counts(
        &self,
        account_id: &str,
        storefront: Storefront,
        product_id: &str,
    ) -> Result<(usize, usize)> {
        let (unlocked, total) = self
            .conn
            .query_row(
                "SELECT COALESCE(SUM(unlocked), 0), COUNT(*) FROM achievement_cache
                 WHERE account_id = ?1 AND storefront = ?2 AND product_id = ?3",
                params![account_id, storefront as i32, product_id],
                |row| Ok((row.get::<_, i64>(0)? as usize, row.get::<_, i64>(1)? as usize)),
            )
            .map_err(Error::from)?;
        tracing::debug!(
            account_id,
            ?storefront,
            product_id,
            unlocked,
            total,
            "read achievement counts"
        );
        Ok((unlocked, total))
    }

    /// Every cached achievement for one game, locked and unlocked alike.
    /// Unlocked first, then alphabetical.
    pub fn list_achievements(
        &self,
        account_id: &str,
        storefront: Storefront,
        product_id: &str,
    ) -> Result<Vec<CachedAchievement>> {
        let mut stmt = self.conn.prepare(
            "SELECT achievement_key, name, description, icon_unlocked_url, icon_locked_url, unlocked, unlocked_at
             FROM achievement_cache
             WHERE account_id = ?1 AND storefront = ?2 AND product_id = ?3
             ORDER BY unlocked DESC, name ASC",
        )?;
        let rows = stmt.query_map(params![account_id, storefront as i32, product_id], |row| {
            Ok(CachedAchievement {
                achievement_key: row.get(0)?,
                name: row.get(1)?,
                description: row.get(2)?,
                icon_unlocked_url: row.get(3)?,
                icon_locked_url: row.get(4)?,
                unlocked: row.get(5)?,
                unlocked_at: row.get(6)?,
            })
        })?;
        let achievements = rows.collect::<rusqlite::Result<Vec<_>>>().map_err(Error::from)?;
        tracing::debug!(
            account_id,
            ?storefront,
            product_id,
            achievement_count = achievements.len(),
            "listed achievements"
        );
        Ok(achievements)
    }

    /// Upserts the latest known achievement list for one game and returns
    /// the ones that just transitioned from locked/unseen to unlocked —
    /// the set worth notifying about.
    pub fn upsert_achievements(
        &mut self,
        account_id: &str,
        storefront: Storefront,
        product_id: &str,
        items: &[CachedAchievement],
    ) -> Result<Vec<CachedAchievement>> {
        let tx = self.conn.transaction()?;
        let mut newly_unlocked = Vec::new();

        for item in items {
            let was_unlocked: Option<bool> = tx
                .query_row(
                    "SELECT unlocked FROM achievement_cache
                     WHERE account_id = ?1 AND storefront = ?2 AND product_id = ?3 AND achievement_key = ?4",
                    params![account_id, storefront as i32, product_id, item.achievement_key],
                    |row| row.get(0),
                )
                .optional()?;

            // One row per achievement per game — trace, not debug, since a
            // single sync can walk hundreds of these and each one alone is
            // rarely worth a line at the default debug level.
            tracing::trace!(
                achievement_key = %item.achievement_key,
                unlocked = item.unlocked,
                "upserting achievement row"
            );

            if item.unlocked && was_unlocked != Some(true) {
                tracing::info!(
                    account_id,
                    ?storefront,
                    product_id,
                    achievement_key = %item.achievement_key,
                    "achievement unlocked"
                );
                newly_unlocked.push(item.clone());
            }

            tx.execute(
                "INSERT INTO achievement_cache (account_id, storefront, product_id, achievement_key, name, description, icon_unlocked_url, icon_locked_url, unlocked, unlocked_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
                 ON CONFLICT(account_id, storefront, product_id, achievement_key) DO UPDATE SET
                    name = excluded.name,
                    description = excluded.description,
                    icon_unlocked_url = excluded.icon_unlocked_url,
                    icon_locked_url = excluded.icon_locked_url,
                    unlocked = excluded.unlocked,
                    unlocked_at = excluded.unlocked_at",
                params![
                    account_id,
                    storefront as i32,
                    product_id,
                    item.achievement_key,
                    item.name,
                    item.description,
                    item.icon_unlocked_url,
                    item.icon_locked_url,
                    item.unlocked,
                    item.unlocked_at,
                ],
            )?;
        }

        tx.commit()?;
        tracing::debug!(
            account_id,
            ?storefront,
            product_id,
            item_count = items.len(),
            newly_unlocked = newly_unlocked.len(),
            "upserted achievements"
        );
        Ok(newly_unlocked)
    }

    /// Marks one already-cached achievement unlocked from decanter-hook's
    /// own local record, for a game whose real `Galaxy64.dll` never gets
    /// to tell GOG's servers -- there's no Galaxy client under Wine to
    /// relay it, so GOG's own achievement API never learns about it
    /// either, and a normal [`Self::upsert_achievements`] sync would keep
    /// reporting it locked forever.
    ///
    /// Update-only, deliberately: an insert would need a `name` this
    /// call site never has (the hook only ever sees the raw achievement
    /// key, not its display metadata). Returns whether a row actually
    /// matched -- `false` means this game's achievement list was never
    /// synced from GOG yet, so there was nothing to mark; the caller is
    /// expected to run this again after the next real sync.
    pub fn mark_local_unlock(
        &self,
        account_id: &str,
        storefront: Storefront,
        product_id: &str,
        achievement_key: &str,
        unlocked_at: &str,
    ) -> Result<bool> {
        let updated = self.conn.execute(
            "UPDATE achievement_cache SET unlocked = 1, unlocked_at = ?5
             WHERE account_id = ?1 AND storefront = ?2 AND product_id = ?3 AND achievement_key = ?4
                   AND unlocked = 0",
            params![account_id, storefront as i32, product_id, achievement_key, unlocked_at],
        )?;
        tracing::debug!(
            account_id,
            ?storefront,
            product_id,
            achievement_key,
            matched = updated > 0,
            "applied a local (hook-reported) achievement unlock"
        );
        Ok(updated > 0)
    }

    /// Records one hook-installation failure so it can be reviewed later
    /// instead of only ever existing inside a game's own, ephemeral log
    /// file.
    pub fn record_hook_diagnostic(
        &self,
        account_id: &str,
        storefront: Storefront,
        product_id: &str,
        diagnostic: &HookDiagnostic,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT INTO hook_diagnostics (account_id, storefront, product_id, recorded_at, stage, detail)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(account_id, storefront, product_id, recorded_at, stage) DO NOTHING",
            params![
                account_id,
                storefront as i32,
                product_id,
                diagnostic.recorded_at,
                diagnostic.stage,
                diagnostic.detail,
            ],
        )?;
        tracing::warn!(
            account_id,
            ?storefront,
            product_id,
            stage = diagnostic.stage,
            detail = diagnostic.detail,
            "decanter-hook failed to install for this game"
        );
        Ok(())
    }

    /// Every recorded hook failure for one game, newest first -- what a
    /// developer adapting `abi_scan` for a new `Galaxy64.dll` shape would
    /// pull up.
    pub fn list_hook_diagnostics(
        &self,
        account_id: &str,
        storefront: Storefront,
        product_id: &str,
    ) -> Result<Vec<HookDiagnostic>> {
        let mut stmt = self.conn.prepare(
            "SELECT recorded_at, stage, detail FROM hook_diagnostics
             WHERE account_id = ?1 AND storefront = ?2 AND product_id = ?3
             ORDER BY recorded_at DESC",
        )?;
        let rows = stmt.query_map(params![account_id, storefront as i32, product_id], |row| {
            Ok(HookDiagnostic {
                recorded_at: row.get(0)?,
                stage: row.get(1)?,
                detail: row.get(2)?,
            })
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>().map_err(Error::from)
    }

    /// Every product id's `(unlocked, total)` achievement counts for an
    /// account, in one query — same reasoning as [`Db::all_playtime`].
    pub fn all_achievement_counts(
        &self,
        account_id: &str,
        storefront: Storefront,
    ) -> Result<std::collections::HashMap<String, (usize, usize)>> {
        let mut stmt = self.conn.prepare(
            "SELECT product_id, COALESCE(SUM(unlocked), 0), COUNT(*) FROM achievement_cache
             WHERE account_id = ?1 AND storefront = ?2 GROUP BY product_id",
        )?;
        let rows = stmt.query_map(params![account_id, storefront as i32], |row| {
            Ok((
                row.get::<_, String>(0)?,
                (row.get::<_, i64>(1)? as usize, row.get::<_, i64>(2)? as usize),
            ))
        })?;
        let counts = rows
            .collect::<rusqlite::Result<std::collections::HashMap<_, _>>>()
            .map_err(Error::from)?;
        tracing::debug!(
            account_id,
            ?storefront,
            count = counts.len(),
            "read all achievement counts"
        );
        Ok(counts)
    }
}
