//! Local cache, all of it keyed by `(account_id, storefront, product_id)`
//! — achievements and library metadata both belong to the storefront
//! account that owns them, not to any particular install and not
//! directly to the [`crate::profiles`] a person switches between (a
//! profile groups several accounts, see [`crate::accounts`]). Where a
//! game is installed on disk is [`crate::library`]'s concern, read
//! straight off its manifests — this store never duplicates it.

use std::path::{Path, PathBuf};

use rusqlite::{Connection, OptionalExtension, params};

use crate::catalog::Storefront;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
}

pub type Result<T> = std::result::Result<T, Error>;

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
}

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

/// Self-tracked: nothing upstream reports play sessions, so
/// `seconds_total` accumulates whatever `decanter` itself measured between a
/// launch it made and the process exiting.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Playtime {
    pub seconds_total: i64,
    pub last_played_at: Option<i64>,
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

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS games_cache (
    account_id      TEXT NOT NULL,
    storefront      INTEGER NOT NULL,
    product_id      TEXT NOT NULL,
    title           TEXT NOT NULL,
    icon_url        TEXT,
    cover_url       TEXT,
    background_url  TEXT,
    kind            TEXT NOT NULL DEFAULT 'game',
    purchased_at    INTEGER,
    release_date    INTEGER,
    rating          TEXT,
    genres          TEXT,
    library_rank    INTEGER,
    parent_id       TEXT,
    installable     INTEGER NOT NULL DEFAULT 1,
    first_seen      INTEGER NOT NULL DEFAULT 0,
    updated_at      INTEGER NOT NULL,
    PRIMARY KEY (account_id, storefront, product_id)
);

CREATE TABLE IF NOT EXISTS achievement_cache (
    account_id        TEXT NOT NULL,
    storefront        INTEGER NOT NULL,
    product_id        TEXT NOT NULL,
    achievement_key   TEXT NOT NULL,
    name              TEXT NOT NULL,
    description       TEXT,
    icon_unlocked_url TEXT,
    icon_locked_url   TEXT,
    unlocked          INTEGER NOT NULL DEFAULT 0,
    unlocked_at       TEXT,
    PRIMARY KEY (account_id, storefront, product_id, achievement_key)
);

CREATE TABLE IF NOT EXISTS hook_diagnostics (
    account_id  TEXT NOT NULL,
    storefront  INTEGER NOT NULL,
    product_id  TEXT NOT NULL,
    recorded_at INTEGER NOT NULL,
    stage       TEXT NOT NULL,
    detail      TEXT NOT NULL,
    PRIMARY KEY (account_id, storefront, product_id, recorded_at, stage)
);

CREATE TABLE IF NOT EXISTS favorites (
    account_id  TEXT NOT NULL,
    storefront  INTEGER NOT NULL,
    product_id  TEXT NOT NULL,
    PRIMARY KEY (account_id, storefront, product_id)
);

CREATE TABLE IF NOT EXISTS settings (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS profiles (
    id          TEXT PRIMARY KEY,
    name        TEXT NOT NULL,
    created_at  INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS accounts (
    id          TEXT NOT NULL,
    storefront  INTEGER NOT NULL,
    profile_id  TEXT NOT NULL,
    name        TEXT NOT NULL,
    created_at  INTEGER NOT NULL,
    PRIMARY KEY (id, storefront)
);

CREATE TABLE IF NOT EXISTS playtime (
    account_id      TEXT NOT NULL,
    storefront      INTEGER NOT NULL,
    product_id      TEXT NOT NULL,
    seconds_total   INTEGER NOT NULL DEFAULT 0,
    last_played_at  INTEGER,
    PRIMARY KEY (account_id, storefront, product_id)
);

CREATE TABLE IF NOT EXISTS collections (
    id          TEXT PRIMARY KEY,
    profile_id  TEXT NOT NULL,
    name        TEXT NOT NULL,
    filter      TEXT,
    position    INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS collection_members (
    collection_id  TEXT NOT NULL,
    storefront     INTEGER NOT NULL,
    product_id     TEXT NOT NULL,
    PRIMARY KEY (collection_id, storefront, product_id)
);

CREATE TABLE IF NOT EXISTS hidden (
    account_id  TEXT NOT NULL,
    storefront  INTEGER NOT NULL,
    product_id  TEXT NOT NULL,
    PRIMARY KEY (account_id, storefront, product_id)
);

CREATE TABLE IF NOT EXISTS profile_libraries (
    profile_id  TEXT NOT NULL,
    path        TEXT NOT NULL,
    added_at    INTEGER NOT NULL,
    PRIMARY KEY (profile_id, path)
);

CREATE TABLE IF NOT EXISTS steam_shortcuts (
    account_id  TEXT NOT NULL,
    storefront  INTEGER NOT NULL,
    product_id  TEXT NOT NULL,
    appid       INTEGER NOT NULL,
    PRIMARY KEY (account_id, storefront, product_id)
);

DROP TABLE IF EXISTS ps_scan_dirs;
DROP TABLE IF EXISTS game_links;
";

/// Where `decanter-sidecar` and the UI both look for this cache — kept in one
/// place so the two binaries can never end up pointed at different files.
pub fn default_db_path() -> PathBuf {
    let dir = crate::data_dir();
    std::fs::create_dir_all(&dir).expect("creating decanter data dir");
    tracing::debug!(dir = %dir.display(), "ensured decanter data dir exists");
    dir.join("links.db")
}

pub struct Db {
    pub(crate) conn: Connection,
}

/// Guards the one-time reset in [`Db::reset_unbacked_first_seen`].
const FIRST_SEEN_MIGRATION_SETTING: &str = "first_seen_migration_v1";

/// Whether `table` has been created yet — `pragma_table_info` alone can't
/// tell a genuinely missing table from one that's just missing a column,
/// since both return zero rows.
fn table_exists(conn: &Connection, table: &str) -> Result<bool> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
        [table],
        |row| row.get(0),
    )?;
    Ok(count > 0)
}

/// Whether `table` has a column named `column`. Only meaningful once
/// [`table_exists`] has confirmed the table is actually there.
fn has_column(conn: &Connection, table: &str, column: &str) -> Result<bool> {
    let count: i64 = conn.query_row(
        &format!("SELECT COUNT(*) FROM pragma_table_info('{table}') WHERE name = ?1"),
        [column],
        |row| row.get(0),
    )?;
    Ok(count > 0)
}

impl Db {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        tracing::debug!(path = %path.display(), "opening db");
        let conn = Connection::open(path)?;
        // `achievement_cache` used to be keyed by program_id, and
        // `games_cache` had no catalog metadata columns; `CREATE TABLE IF
        // NOT EXISTS` leaves an old-shaped table alone, so a real
        // incompatible schema change needs an explicit drop first. Local
        // single-user cache — safe to just rebuild rather than migrate rows.
        // Gated on the table actually existing first: `pragma_table_info`
        // on a table that was never created also returns zero rows, same
        // as one that's missing the column — without the gate, a brand
        // new db logged (and "rebuilt") tables that were never there to
        // begin with.
        if table_exists(&conn, "achievement_cache")? && !has_column(&conn, "achievement_cache", "account_id")? {
            tracing::info!(
                table = "achievement_cache",
                "old-shaped table found, dropping and rebuilding"
            );
            conn.execute_batch("DROP TABLE IF EXISTS achievement_cache;")?;
        }
        if table_exists(&conn, "games_cache")? && !has_column(&conn, "games_cache", "kind")? {
            tracing::info!(table = "games_cache", "old-shaped table found, dropping and rebuilding");
            conn.execute_batch("DROP TABLE IF EXISTS games_cache;")?;
        }
        // `background_url` was added after `games_cache` shipped — a table
        // this misses it on was created before, and never got the hero art
        // column at all, so the detail pane fell back to `cover_url` (or
        // nothing) for every cached row until the next live fetch overwrote
        // it. Same drop-and-rebuild as above, not a row migration.
        if table_exists(&conn, "games_cache")? && !has_column(&conn, "games_cache", "background_url")? {
            tracing::info!(
                table = "games_cache",
                "table missing background_url column, dropping and rebuilding"
            );
            conn.execute_batch("DROP TABLE IF EXISTS games_cache;")?;
        }
        // These four used to be keyed by `profile_id`, back when a profile
        // and a storefront account were the same thing — now a profile can
        // group several accounts, so the cache has to key by account
        // instead. Same drop-and-rebuild as above, not a row migration.
        for table in ["games_cache", "favorites", "playtime", "hidden"] {
            if table_exists(&conn, table)? && !has_column(&conn, table, "account_id")? {
                tracing::info!(table, "table missing account_id column, dropping and rebuilding");
                conn.execute_batch(&format!("DROP TABLE IF EXISTS {table};"))?;
            }
        }

        conn.execute_batch(SCHEMA)?;
        tracing::debug!("applied schema");

        // Additive, unlike the drop-and-rebuild migrations above: `profiles`
        // holds real user data (which accounts are grouped where), not a
        // cache, so a missing column gets a column, never a dropped table.
        let profiles_has_library_path: i64 = conn.query_row(
            "SELECT COUNT(*) FROM pragma_table_info('profiles') WHERE name = 'library_path'",
            [],
            |row| row.get(0),
        )?;
        if profiles_has_library_path == 0 {
            tracing::info!(table = "profiles", "adding library_path column");
            conn.execute_batch("ALTER TABLE profiles ADD COLUMN library_path TEXT;")?;
        }

        let db = Self { conn };
        db.reset_unbacked_first_seen()?;
        tracing::debug!(path = %path.display(), "db opened");
        Ok(db)
    }

    /// One-time cleanup for `first_seen` values a version before
    /// `confirmed_new` existed stamped indiscriminately. Once, because
    /// after this a legitimately-stamped row (no purchase or release date,
    /// `first_seen` from a real confirmed-new insert) is indistinguishable
    /// from a stale one — running this more than once would erase those too.
    fn reset_unbacked_first_seen(&self) -> Result<()> {
        if self.setting(FIRST_SEEN_MIGRATION_SETTING)?.is_some() {
            return Ok(());
        }
        let rows_reset = self.conn.execute(
            "UPDATE games_cache SET first_seen = 0
             WHERE first_seen != 0 AND purchased_at IS NULL AND release_date IS NULL",
            [],
        )?;
        tracing::info!(rows_reset, "reset unbacked first_seen stamps (one-time migration)");
        self.set_setting(FIRST_SEEN_MIGRATION_SETTING, "1")
    }

    /// Records or refreshes one game's catalog entry.
    ///
    /// `confirmed_new` is only consulted for a brand-new row: it stamps
    /// `first_seen` with now when neither `purchased_at` nor `release_date`
    /// is known, so the game still sorts as recently added. A later refresh
    /// never touches `first_seen` again, confirmed or not.
    ///
    /// A `None`/empty optional field (`purchased_at`, `release_date`,
    /// `rating`, `genres`, `library_rank`, `parent_id`) means "this fetch
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
                    parent_id, installable, first_seen, updated_at
                 )
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, {first_seen_if_new}, strftime('%s','now'))
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
                    parent_id, installable
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

    pub fn is_favorite(&self, account_id: &str, storefront: Storefront, product_id: &str) -> Result<bool> {
        let is_favorite = self
            .conn
            .query_row(
                "SELECT 1 FROM favorites WHERE account_id = ?1 AND storefront = ?2 AND product_id = ?3",
                params![account_id, storefront as i32, product_id],
                |_| Ok(()),
            )
            .optional()
            .map(|row| row.is_some())
            .map_err(Error::from)?;
        tracing::debug!(
            account_id,
            ?storefront,
            product_id,
            is_favorite,
            "checked favorite status"
        );
        Ok(is_favorite)
    }

    /// Every favorited product id for an account — the sidebar's
    /// "Favoritos" filter checks membership in this set.
    pub fn list_favorite_ids(
        &self,
        account_id: &str,
        storefront: Storefront,
    ) -> Result<std::collections::HashSet<String>> {
        let mut stmt = self
            .conn
            .prepare("SELECT product_id FROM favorites WHERE account_id = ?1 AND storefront = ?2")?;
        let rows = stmt.query_map(params![account_id, storefront as i32], |row| row.get(0))?;
        let ids = rows
            .collect::<rusqlite::Result<std::collections::HashSet<_>>>()
            .map_err(Error::from)?;
        tracing::debug!(account_id, ?storefront, count = ids.len(), "listed favorite ids");
        Ok(ids)
    }

    pub fn set_favorite(
        &self,
        account_id: &str,
        storefront: Storefront,
        product_id: &str,
        favorite: bool,
    ) -> Result<()> {
        if favorite {
            self.conn.execute(
                "INSERT OR IGNORE INTO favorites (account_id, storefront, product_id) VALUES (?1, ?2, ?3)",
                params![account_id, storefront as i32, product_id],
            )?;
        } else {
            self.conn.execute(
                "DELETE FROM favorites WHERE account_id = ?1 AND storefront = ?2 AND product_id = ?3",
                params![account_id, storefront as i32, product_id],
            )?;
        }
        tracing::debug!(account_id, ?storefront, product_id, favorite, "set favorite state");
        Ok(())
    }

    /// The Steam `shortcuts.vdf` appid decanter assigned this game, if it's
    /// ever been exported — the removal path's only way to find the right
    /// entry and grid artwork without re-deriving anything from the
    /// game's (possibly since-changed) title.
    pub fn steam_shortcut_appid(
        &self,
        account_id: &str,
        storefront: Storefront,
        product_id: &str,
    ) -> Result<Option<i32>> {
        let appid = self
            .conn
            .query_row(
                "SELECT appid FROM steam_shortcuts WHERE account_id = ?1 AND storefront = ?2 AND product_id = ?3",
                params![account_id, storefront as i32, product_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(Error::from)?;
        tracing::debug!(
            account_id,
            ?storefront,
            product_id,
            found = appid.is_some(),
            "read steam shortcut appid"
        );
        Ok(appid)
    }

    pub fn set_steam_shortcut_appid(
        &self,
        account_id: &str,
        storefront: Storefront,
        product_id: &str,
        appid: i32,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT INTO steam_shortcuts (account_id, storefront, product_id, appid) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(account_id, storefront, product_id) DO UPDATE SET appid = excluded.appid",
            params![account_id, storefront as i32, product_id, appid],
        )?;
        tracing::debug!(account_id, ?storefront, product_id, appid, "set steam shortcut appid");
        Ok(())
    }

    pub fn remove_steam_shortcut_appid(
        &self,
        account_id: &str,
        storefront: Storefront,
        product_id: &str,
    ) -> Result<()> {
        self.conn.execute(
            "DELETE FROM steam_shortcuts WHERE account_id = ?1 AND storefront = ?2 AND product_id = ?3",
            params![account_id, storefront as i32, product_id],
        )?;
        tracing::debug!(account_id, ?storefront, product_id, "removed steam shortcut appid");
        Ok(())
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

    /// Whether closing the main window should hide it behind a tray icon
    /// instead of quitting — read once at launch (see `ui`'s `main.rs`),
    /// so this is a single global setting, not per-account.
    pub fn close_to_tray(&self) -> Result<bool> {
        let close_to_tray = self.setting("close_to_tray")?.as_deref() == Some("1");
        tracing::debug!(close_to_tray, "read close_to_tray setting");
        Ok(close_to_tray)
    }

    pub fn set_close_to_tray(&self, close_to_tray: bool) -> Result<()> {
        self.set_setting("close_to_tray", if close_to_tray { "1" } else { "0" })
    }

    /// Whether an install/uninstall should also write a real Steam
    /// `shortcuts.vdf` entry for the game — on by default, since that's
    /// the whole point of the feature, but this writes into the actual
    /// Steam user's own config files, so it stays easy to turn off.
    pub fn steam_export_enabled(&self) -> Result<bool> {
        let enabled = self.setting("steam_export_enabled")?.as_deref() != Some("0");
        tracing::debug!(enabled, "read steam_export_enabled setting");
        Ok(enabled)
    }

    pub fn set_steam_export_enabled(&self, enabled: bool) -> Result<()> {
        self.set_setting("steam_export_enabled", if enabled { "1" } else { "0" })
    }

    /// The Proton build to launch with when a game has no per-game choice
    /// of its own — a bare name from `decanter_runtime::steam_runners`, or
    /// `None` for `decanter_runtime::Proton::Latest`.
    pub fn default_proton(&self) -> Result<Option<String>> {
        let proton = self.setting("default_proton")?;
        tracing::debug!(proton = proton.as_deref(), "read default_proton setting");
        Ok(proton)
    }

    pub fn set_default_proton(&self, proton: Option<&str>) -> Result<()> {
        match proton {
            Some(name) => self.set_setting("default_proton", name),
            None => self.delete_setting("default_proton"),
        }
    }

    /// The UI language's code (`es`, `en`), or `None` to follow the system
    /// locale.
    pub fn language(&self) -> Result<Option<String>> {
        let language = self.setting("language")?;
        tracing::debug!(language = language.as_deref(), "read language setting");
        Ok(language)
    }

    pub fn set_language(&self, language: Option<&str>) -> Result<()> {
        match language {
            Some(code) => self.set_setting("language", code),
            None => self.delete_setting("language"),
        }
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

    /// Arbitrary key/value storage — one-off flags and view state that
    /// don't warrant a table of their own (see [`crate::adopt`] and
    /// [`crate::profiles`] for two callers).
    pub fn setting(&self, key: &str) -> Result<Option<String>> {
        // Called from several other Db methods on essentially every
        // operation (active profile, close_to_tray, default_proton, ...),
        // so this alone would flood debug level — trace instead.
        let value = self
            .conn
            .query_row("SELECT value FROM settings WHERE key = ?1", params![key], |row| {
                row.get(0)
            })
            .optional()
            .map_err(Error::from)?;
        tracing::trace!(key, found = value.is_some(), "read setting");
        Ok(value)
    }

    pub fn set_setting(&self, key: &str, value: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO settings (key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![key, value],
        )?;
        tracing::debug!(key, value, "set setting");
        Ok(())
    }

    /// Reverts a setting to unset, distinct from setting it to an empty
    /// string — needed wherever `None` means "use the built-in default"
    /// rather than "the empty string".
    pub fn delete_setting(&self, key: &str) -> Result<()> {
        self.conn.execute("DELETE FROM settings WHERE key = ?1", params![key])?;
        tracing::debug!(key, "deleted setting");
        Ok(())
    }
}
