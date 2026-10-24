//! Local cache, all of it keyed by `(account_id, storefront, product_id)`
//! — achievements and library metadata both belong to the storefront
//! account that owns them, not to any particular install and not
//! directly to the [`crate::profiles`] a person switches between (a
//! profile groups several accounts, see [`crate::accounts`]). Where a
//! game is installed on disk is [`crate::library`]'s concern, read
//! straight off its manifests — this store never duplicates it.

mod achievements;
mod dump_sizes;
mod favorites;
mod game_options;
mod games;
mod library_caches;
mod playtime;
mod settings;
mod steam_shortcuts;

use std::path::{Path, PathBuf};

use rusqlite::{Connection, OpenFlags};

pub use self::{
    achievements::{CachedAchievement, HookDiagnostic},
    games::CachedGame,
    playtime::Playtime,
};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
    #[error("the library at {} isn't there", .0.display())]
    LibraryUnavailable(PathBuf),
    #[error("no library has been set up yet")]
    NoLibrary,
}

pub type Result<T> = std::result::Result<T, Error>;

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
    has_macos_build INTEGER,
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

CREATE TABLE IF NOT EXISTS game_options (
    game_key  TEXT NOT NULL,
    option    TEXT NOT NULL,
    enabled   INTEGER NOT NULL,
    PRIMARY KEY (game_key, option)
);

CREATE TABLE IF NOT EXISTS dump_sizes (
    dir          TEXT PRIMARY KEY,
    size         INTEGER NOT NULL,
    fingerprint  TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS library_caches (
    folder      TEXT PRIMARY KEY,
    cache_path  TEXT NOT NULL
);

DROP TABLE IF EXISTS ps_scan_dirs;
DROP TABLE IF EXISTS game_links;
";

pub struct Db {
    pub(crate) conn: Connection,
}

/// Guards the one-time reset in [`Db::reset_unbacked_first_seen`].
const FIRST_SEEN_MIGRATION_SETTING: &str = "first_seen_migration_v1";

/// Whether `table` has been created yet — `pragma_table_info` alone can't
/// tell a genuinely missing table from one that's just missing a column,
/// since both return zero rows.
pub(crate) fn table_exists(conn: &Connection, table: &str) -> Result<bool> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
        [table],
        |row| row.get(0),
    )?;
    Ok(count > 0)
}

/// Whether `table` has a column named `column`. Only meaningful once
/// [`table_exists`] has confirmed the table is actually there.
pub(crate) fn has_column(conn: &Connection, table: &str, column: &str) -> Result<bool> {
    let count: i64 = conn.query_row(
        &format!("SELECT COUNT(*) FROM pragma_table_info('{table}') WHERE name = ?1"),
        [column],
        |row| row.get(0),
    )?;
    Ok(count > 0)
}

impl Db {
    /// The active library's database (see [`crate::bootstrap`]).
    pub fn open_active() -> Result<Self> {
        match crate::bootstrap::active_db_path() {
            Some(path) => Self::open(path),
            None => Err(crate::bootstrap::unavailable_root().map_or(Error::NoLibrary, Error::LibraryUnavailable)),
        }
    }

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

        // Additive, so a database an older decanter still opens keeps its
        // cached library: that one names its columns and never reads this.
        if !has_column(&conn, "games_cache", "has_macos_build")? {
            tracing::info!(table = "games_cache", "adding has_macos_build column");
            conn.execute_batch("ALTER TABLE games_cache ADD COLUMN has_macos_build INTEGER;")?;
        }

        let db = Self { conn };
        db.reset_unbacked_first_seen()?;
        tracing::debug!(path = %path.display(), "db opened");
        Ok(db)
    }

    /// Another library's database, for reading only: its schema is never
    /// touched, since a decanter that has it as its active library may be
    /// using it.
    pub fn open_read_only(path: impl AsRef<Path>) -> Result<Self> {
        let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        Ok(Self { conn })
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
}
