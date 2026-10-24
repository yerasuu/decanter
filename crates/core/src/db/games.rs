//! The cached catalog: one row per owned product, per account.

use rusqlite::{OptionalExtension as _, params};

use super::{Db, Error, Result};
use crate::catalog::Storefront;

#[derive(Debug, Clone, PartialEq)]
pub struct CachedGame {
    pub storefront: Storefront,
    pub product_id: String,
    pub title: String,
    pub icon_url: Option<String>,
    pub cover_url: Option<String>,
    pub background_url: Option<String>,
    pub kind: String,
    pub purchased_at: Option<i64>,
    pub release_date: Option<i64>,
    pub rating: Option<String>,
    /// Empty means "the store didn't say" on this fetch, not "this game has
    /// no genres" — a quiet [`Db::upsert_game`] refresh leaves whatever was
    /// cached before untouched rather than blanking it out.
    pub genres: Vec<String>,
    /// GOG's own added-to-library order, 0 = most recent. `None` when this
    /// fetch didn't carry a rank — same "leave it alone" rule as `genres`.
    pub library_rank: Option<i64>,
    /// The base game's product id, for DLC. `None` for a base game.
    pub parent_id: Option<String>,
    pub installable: bool,
    /// `None` when this fetch didn't carry it — same "leave it alone" rule
    /// as `genres`.
    pub has_macos_build: Option<bool>,
}

/// Unit separator, not a comma: a genre name is free text from the store
/// and can't be assumed comma-free.
const GENRE_SEPARATOR: char = '\u{1f}';

/// `None` for an empty list, so `upsert_game`'s `COALESCE(excluded.genres,
/// genres)` leaves a previous, real genre list alone on a quiet refresh
/// instead of overwriting it with nothing.
fn join_genres(genres: &[String]) -> Option<String> {
    (!genres.is_empty()).then(|| genres.join(&GENRE_SEPARATOR.to_string()))
}

fn split_genres(raw: Option<String>) -> Vec<String> {
    raw.map(|raw| raw.split(GENRE_SEPARATOR).map(str::to_owned).collect())
        .unwrap_or_default()
}

impl Db {
    /// Records or refreshes one game's catalog entry.
    ///
    /// `confirmed_new` is only consulted for a brand-new row: it stamps
    /// `first_seen` with now when neither `purchased_at` nor `release_date`
    /// is known, so the game still sorts as recently added. A later refresh
    /// never touches `first_seen` again, confirmed or not.
    ///
    /// A `None`/empty optional field (`purchased_at`, `release_date`,
    /// `rating`, `genres`, `library_rank`, `parent_id`, `has_macos_build`) means "this fetch
    /// didn't carry it", not "clear it" — a refresh where the per-game
    /// lookup failed must not blank out catalog data an earlier, successful
    /// one found.
    pub fn upsert_game(&self, account_id: &str, game: &CachedGame, confirmed_new: bool) -> Result<()> {
        let first_seen_if_new = if confirmed_new && game.purchased_at.is_none() && game.release_date.is_none() {
            "strftime('%s','now')"
        } else {
            "0"
        };
        self.conn.execute(
            &format!(
                "INSERT INTO games_cache (
                    account_id, storefront, product_id, title, icon_url, cover_url, background_url,
                    kind, purchased_at, release_date, rating, genres, library_rank,
                    parent_id, installable, has_macos_build, first_seen, updated_at
                 )
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, {first_seen_if_new}, strftime('%s','now'))
                 ON CONFLICT(account_id, storefront, product_id) DO UPDATE SET
                    title = excluded.title,
                    icon_url = excluded.icon_url,
                    cover_url = excluded.cover_url,
                    background_url = excluded.background_url,
                    kind = excluded.kind,
                    purchased_at = COALESCE(excluded.purchased_at, purchased_at),
                    release_date = COALESCE(excluded.release_date, release_date),
                    rating = COALESCE(excluded.rating, rating),
                    genres = COALESCE(excluded.genres, genres),
                    library_rank = COALESCE(excluded.library_rank, library_rank),
                    parent_id = COALESCE(excluded.parent_id, parent_id),
                    installable = excluded.installable,
                    has_macos_build = COALESCE(excluded.has_macos_build, has_macos_build),
                    updated_at = excluded.updated_at"
            ),
            params![
                account_id,
                game.storefront as i32,
                game.product_id,
                game.title,
                game.icon_url,
                game.cover_url,
                game.background_url,
                game.kind,
                game.purchased_at,
                game.release_date,
                game.rating,
                join_genres(&game.genres),
                game.library_rank,
                game.parent_id,
                game.installable,
                game.has_macos_build,
            ],
        )?;
        tracing::debug!(
            account_id,
            storefront = ?game.storefront,
            product_id = %game.product_id,
            confirmed_new,
            "upserted cached game"
        );
        Ok(())
    }

    /// Whether this account has ever had a game cached — lets a caller tell
    /// a first-ever sync apart from one that's merely empty right now.
    pub fn has_games(&self, account_id: &str) -> Result<bool> {
        let has_games = self
            .conn
            .query_row(
                "SELECT 1 FROM games_cache WHERE account_id = ?1 LIMIT 1",
                params![account_id],
                |_| Ok(()),
            )
            .optional()
            .map(|row| row.is_some())
            .map_err(Error::from)?;
        tracing::debug!(account_id, has_games, "checked for cached games");
        Ok(has_games)
    }

    /// Drops every cached game for `(account_id, storefront)` not in
    /// `keep_ids`, and returns how many were dropped.
    ///
    /// Refuses an empty `keep_ids` rather than obeying it: an empty listing
    /// means the fetch itself failed, not that the account was emptied out.
    pub fn retain_games(&self, account_id: &str, storefront: Storefront, keep_ids: &[String]) -> Result<usize> {
        if keep_ids.is_empty() {
            tracing::warn!(
                account_id,
                ?storefront,
                "refusing to retain against an empty keep list, treating it as a failed fetch"
            );
            return Ok(0);
        }
        let placeholders = keep_ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
        let mut stmt = self.conn.prepare(&format!(
            "DELETE FROM games_cache WHERE account_id = ? AND storefront = ? AND product_id NOT IN ({placeholders})"
        ))?;
        let storefront_value = storefront as i32;
        let mut bound: Vec<&dyn rusqlite::ToSql> = Vec::with_capacity(2 + keep_ids.len());
        bound.push(&account_id);
        bound.push(&storefront_value);
        for id in keep_ids {
            bound.push(id);
        }
        let dropped = stmt.execute(bound.as_slice()).map_err(Error::from)?;
        tracing::debug!(
            account_id,
            ?storefront,
            keep_count = keep_ids.len(),
            dropped,
            "pruned stale cached games"
        );
        Ok(dropped)
    }

    /// Whatever the last `WatchGames` session cached for this account —
    /// shown immediately on startup so the sidebar isn't empty while a
    /// fresh stream reconnects and re-scans the GOG account.
    pub fn list_games(&self, account_id: &str) -> Result<Vec<CachedGame>> {
        let mut stmt = self.conn.prepare(
            "SELECT storefront, product_id, title, icon_url, cover_url, background_url,
                    kind, purchased_at, release_date, rating, genres, library_rank,
                    parent_id, installable, has_macos_build
             FROM games_cache WHERE account_id = ?1 ORDER BY title ASC",
        )?;
        let rows = stmt.query_map(params![account_id], |row| {
            let storefront_raw: i32 = row.get(0)?;
            let storefront = Storefront::try_from(storefront_raw).unwrap_or_else(|_| {
                tracing::warn!(
                    storefront_raw,
                    "unrecognized storefront in games_cache, defaulting to Unspecified"
                );
                Storefront::Unspecified
            });
            Ok(CachedGame {
                storefront,
                product_id: row.get(1)?,
                title: row.get(2)?,
                icon_url: row.get(3)?,
                cover_url: row.get(4)?,
                background_url: row.get(5)?,
                kind: row.get(6)?,
                purchased_at: row.get(7)?,
                release_date: row.get(8)?,
                rating: row.get(9)?,
                genres: split_genres(row.get(10)?),
                library_rank: row.get(11)?,
                parent_id: row.get(12)?,
                installable: row.get(13)?,
                has_macos_build: row.get(14)?,
            })
        })?;
        let games = rows.collect::<rusqlite::Result<Vec<_>>>().map_err(Error::from)?;
        tracing::debug!(account_id, game_count = games.len(), "listed games");
        Ok(games)
    }

    /// "Date added" per product id: a real purchase date, falling back to
    /// the release date and then to [`CachedGame`]'s own `first_seen`
    /// stamp (0 when neither is known and the row was never confirmed new).
    pub fn all_first_seen(
        &self,
        account_id: &str,
        storefront: Storefront,
    ) -> Result<std::collections::HashMap<String, i64>> {
        let mut stmt = self.conn.prepare(
            "SELECT product_id, COALESCE(purchased_at, release_date, NULLIF(first_seen, 0), 0)
             FROM games_cache WHERE account_id = ?1 AND storefront = ?2",
        )?;
        let rows = stmt.query_map(params![account_id, storefront as i32], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })?;
        let dates = rows
            .collect::<rusqlite::Result<std::collections::HashMap<_, _>>>()
            .map_err(Error::from)?;
        tracing::debug!(account_id, ?storefront, count = dates.len(), "read first-seen dates");
        Ok(dates)
    }

    /// GOG's own added-to-library order, by product id — only the games
    /// that have one.
    pub fn all_library_ranks(
        &self,
        account_id: &str,
        storefront: Storefront,
    ) -> Result<std::collections::HashMap<String, i64>> {
        let mut stmt = self.conn.prepare(
            "SELECT product_id, library_rank FROM games_cache
             WHERE account_id = ?1 AND storefront = ?2 AND library_rank IS NOT NULL",
        )?;
        let rows = stmt.query_map(params![account_id, storefront as i32], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })?;
        let ranks = rows
            .collect::<rusqlite::Result<std::collections::HashMap<_, _>>>()
            .map_err(Error::from)?;
        tracing::debug!(account_id, ?storefront, count = ranks.len(), "read library ranks");
        Ok(ranks)
    }

    /// Drops everything kept about one game — favorite, hidden, playtime,
    /// collection membership, and the caches — for a game that no longer
    /// exists at all, not just one uninstalled: an external game deleted
    /// from the library has no catalog entry to come back from.
    ///
    /// Collection membership isn't keyed by account; `product_id` alone
    /// identifies it within a storefront.
    #[inline]
    pub fn forget_product(&self, account_id: &str, storefront: Storefront, product_id: &str) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        for table in [
            "favorites",
            "hidden",
            "playtime",
            "games_cache",
            "achievement_cache",
            "hook_diagnostics",
            "steam_shortcuts",
        ] {
            tx.execute(
                &format!("DELETE FROM {table} WHERE account_id = ?1 AND storefront = ?2 AND product_id = ?3"),
                params![account_id, storefront as i32, product_id],
            )?;
        }
        tx.execute(
            "DELETE FROM collection_members WHERE storefront = ?1 AND product_id = ?2",
            params![storefront as i32, product_id],
        )?;
        tx.commit()?;
        tracing::debug!(account_id, ?storefront, product_id, "forgot everything about a product");
        Ok(())
    }
}
