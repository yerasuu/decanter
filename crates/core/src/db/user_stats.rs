//! A game's user stats, offline-first: the local tables answer whether or not GOG can be reached,
//! and a refresh from GOG never overwrites a value recorded here and not yet synced back.

use rusqlite::types::{FromSql, FromSqlError, FromSqlResult, ToSql, ToSqlOutput, ValueRef};
use rusqlite::{OptionalExtension as _, Row, params};

use super::{Db, Error, Result};
use crate::catalog::Storefront;

/// Its own step after the main schema, so that it stays apart from the `achievement_cache`
/// changes and either can be reordered.
///
/// `NUMERIC` columns keep an int stat's value an `INTEGER` and a float stat's a `REAL`;
/// `changed` marks a value set locally that no sync has uploaded yet, and a refresh skips it.
pub(super) const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS stat_definitions (
    account_id      TEXT NOT NULL,
    storefront      INTEGER NOT NULL,
    product_id      TEXT NOT NULL,
    stat_key        TEXT NOT NULL,
    stat_id         TEXT NOT NULL,
    kind            TEXT NOT NULL,
    increment_only  INTEGER NOT NULL DEFAULT 0,
    rate_window     REAL,
    default_value   NUMERIC,
    min_value       NUMERIC,
    max_value       NUMERIC,
    max_change      NUMERIC,
    PRIMARY KEY (account_id, storefront, product_id, stat_key)
);

CREATE TABLE IF NOT EXISTS user_stats (
    account_id  TEXT NOT NULL,
    storefront  INTEGER NOT NULL,
    product_id  TEXT NOT NULL,
    stat_key    TEXT NOT NULL,
    value       NUMERIC NOT NULL,
    changed     INTEGER NOT NULL DEFAULT 0,
    changed_at  INTEGER,
    fetched_at  INTEGER,
    PRIMARY KEY (account_id, storefront, product_id, stat_key)
);

CREATE TABLE IF NOT EXISTS user_stats_fetches (
    account_id  TEXT NOT NULL,
    storefront  INTEGER NOT NULL,
    product_id  TEXT NOT NULL,
    fetched_at  INTEGER NOT NULL,
    PRIMARY KEY (account_id, storefront, product_id)
);
";

/// The three stat types of the Galaxy SDK's `IStats`
/// ([Galaxy SDK: IStats](https://docs.gog.com/galaxyapi/classgalaxy_1_1api_1_1IStats.html)).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatKind {
    Int,
    Float,
    AvgRate,
}

impl StatKind {
    /// GOG's own `type` text, which is also what the tables store.
    #[must_use]
    #[inline]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Int => "int",
            Self::Float => "float",
            Self::AvgRate => "avgrate",
        }
    }

    #[must_use]
    #[inline]
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "int" => Some(Self::Int),
            "float" => Some(Self::Float),
            "avgrate" => Some(Self::AvgRate),
            _ => None,
        }
    }

    const fn holds(self, value: StatValue) -> bool {
        matches!(
            (self, value),
            (Self::Int, StatValue::Int(_)) | (Self::Float | Self::AvgRate, StatValue::Float(_))
        )
    }
}

impl FromSql for StatKind {
    #[inline]
    fn column_result(value: ValueRef<'_>) -> FromSqlResult<Self> {
        Self::from_name(value.as_str()?).ok_or(FromSqlError::InvalidType)
    }
}

/// An int stat is an `int32` in the SDK; float and average-rate stats are floats.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum StatValue {
    Int(i32),
    Float(f64),
}

impl StatValue {
    fn read(row: &Row<'_>, column: usize, kind: StatKind) -> rusqlite::Result<Self> {
        match kind {
            StatKind::Int => row.get(column).map(Self::Int),
            StatKind::Float | StatKind::AvgRate => row.get(column).map(Self::Float),
        }
    }

    fn read_optional(row: &Row<'_>, column: usize, kind: StatKind) -> rusqlite::Result<Option<Self>> {
        match kind {
            StatKind::Int => row.get::<_, Option<i32>>(column).map(|value| value.map(Self::Int)),
            StatKind::Float | StatKind::AvgRate => {
                row.get::<_, Option<f64>>(column).map(|value| value.map(Self::Float))
            }
        }
    }
}

impl ToSql for StatValue {
    #[inline]
    fn to_sql(&self) -> rusqlite::Result<ToSqlOutput<'_>> {
        Ok(match *self {
            Self::Int(value) => ToSqlOutput::from(value),
            Self::Float(value) => ToSqlOutput::from(value),
        })
    }
}

/// What GOG says a stat is; the bounds are the SDK's to enforce, kept here so that a local
/// service can answer without GOG.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct StatDefinition {
    pub stat_key: String,
    pub stat_id: String,
    pub kind: StatKind,
    pub increment_only: bool,
    pub window: Option<f64>,
    pub default_value: Option<StatValue>,
    pub min_value: Option<StatValue>,
    pub max_value: Option<StatValue>,
    pub max_change: Option<StatValue>,
}

impl StatDefinition {
    #[must_use]
    #[inline]
    pub const fn new(stat_key: String, stat_id: String, kind: StatKind) -> Self {
        Self {
            stat_key,
            stat_id,
            kind,
            increment_only: false,
            window: None,
            default_value: None,
            min_value: None,
            max_value: None,
            max_change: None,
        }
    }
}

/// A stat with the value the local tables hold for it. `is_changed` means that value was set
/// locally and no sync has uploaded it yet.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct UserStat {
    pub definition: StatDefinition,
    pub value: StatValue,
    pub is_changed: bool,
}

impl Db {
    /// Stores what GOG answered for one game's stats and stamps the game as fetched. A value
    /// still marked changed keeps the local one, since it is progress GOG has not seen; its
    /// definition is refreshed all the same. Returns how many values were kept that way.
    ///
    /// Upsert only: a stat GOG stops listing stays, as with [`Db::upsert_achievements`].
    ///
    /// # Errors
    ///
    /// The database refusing the write; nothing is stored then.
    #[inline]
    pub fn refresh_user_stats(
        &mut self,
        account_id: &str,
        storefront: Storefront,
        product_id: &str,
        fetched: &[(StatDefinition, StatValue)],
    ) -> Result<usize> {
        let tx = self.conn.transaction()?;
        let mut kept_local = 0_usize;
        for (definition, value) in fetched {
            tx.execute(
                "INSERT INTO stat_definitions (account_id, storefront, product_id, stat_key, stat_id, kind,
                    increment_only, rate_window, default_value, min_value, max_value, max_change)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
                 ON CONFLICT(account_id, storefront, product_id, stat_key) DO UPDATE SET
                    stat_id = excluded.stat_id,
                    kind = excluded.kind,
                    increment_only = excluded.increment_only,
                    rate_window = excluded.rate_window,
                    default_value = excluded.default_value,
                    min_value = excluded.min_value,
                    max_value = excluded.max_value,
                    max_change = excluded.max_change",
                params![
                    account_id,
                    storefront as i32,
                    product_id,
                    definition.stat_key,
                    definition.stat_id,
                    definition.kind.name(),
                    definition.increment_only,
                    definition.window,
                    definition.default_value,
                    definition.min_value,
                    definition.max_value,
                    definition.max_change,
                ],
            )?;
            let written = tx.execute(
                "INSERT INTO user_stats (account_id, storefront, product_id, stat_key, value, fetched_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, strftime('%s','now'))
                 ON CONFLICT(account_id, storefront, product_id, stat_key) DO UPDATE SET
                    value = excluded.value,
                    fetched_at = excluded.fetched_at
                 WHERE user_stats.changed = 0",
                params![account_id, storefront as i32, product_id, definition.stat_key, value],
            )?;
            if written == 0 {
                tracing::debug!(
                    account_id,
                    ?storefront,
                    product_id,
                    stat_key = %definition.stat_key,
                    "kept the local value of a stat not synced yet"
                );
                kept_local = kept_local.saturating_add(1);
            }
        }
        tx.execute(
            "INSERT INTO user_stats_fetches (account_id, storefront, product_id, fetched_at)
             VALUES (?1, ?2, ?3, strftime('%s','now'))
             ON CONFLICT(account_id, storefront, product_id) DO UPDATE SET fetched_at = excluded.fetched_at",
            params![account_id, storefront as i32, product_id],
        )?;
        tx.commit()?;
        tracing::debug!(
            account_id,
            ?storefront,
            product_id,
            stat_count = fetched.len(),
            kept_local,
            "refreshed user stats"
        );
        Ok(kept_local)
    }

    /// Every stat stored for one game, by key. Empty both for a game without stats and for one
    /// never fetched: [`Db::user_stats_fetched_at`] tells them apart.
    ///
    /// # Errors
    ///
    /// The database failing the read, or a stored row this version cannot read.
    #[inline]
    pub fn list_user_stats(&self, account_id: &str, storefront: Storefront, product_id: &str) -> Result<Vec<UserStat>> {
        let mut stmt = self.conn.prepare(
            "SELECT d.stat_key, d.stat_id, d.kind, d.increment_only, d.rate_window, d.default_value, d.min_value,
                    d.max_value, d.max_change, s.value, s.changed
             FROM stat_definitions d
             JOIN user_stats s USING (account_id, storefront, product_id, stat_key)
             WHERE account_id = ?1 AND storefront = ?2 AND product_id = ?3
             ORDER BY d.stat_key ASC",
        )?;
        let rows = stmt.query_map(params![account_id, storefront as i32, product_id], |row| {
            let kind: StatKind = row.get(2)?;
            Ok(UserStat {
                definition: StatDefinition {
                    stat_key: row.get(0)?,
                    stat_id: row.get(1)?,
                    kind,
                    increment_only: row.get(3)?,
                    window: row.get(4)?,
                    default_value: StatValue::read_optional(row, 5, kind)?,
                    min_value: StatValue::read_optional(row, 6, kind)?,
                    max_value: StatValue::read_optional(row, 7, kind)?,
                    max_change: StatValue::read_optional(row, 8, kind)?,
                },
                value: StatValue::read(row, 9, kind)?,
                is_changed: row.get(10)?,
            })
        })?;
        let stats = rows.collect::<rusqlite::Result<Vec<_>>>().map_err(Error::from)?;
        tracing::debug!(
            account_id,
            ?storefront,
            product_id,
            stat_count = stats.len(),
            "listed user stats"
        );
        Ok(stats)
    }

    /// When GOG last answered this game's stats, in Unix seconds; `None` until it has once.
    ///
    /// # Errors
    ///
    /// The database failing the read.
    #[inline]
    pub fn user_stats_fetched_at(
        &self,
        account_id: &str,
        storefront: Storefront,
        product_id: &str,
    ) -> Result<Option<i64>> {
        self.conn
            .query_row(
                "SELECT fetched_at FROM user_stats_fetches
                 WHERE account_id = ?1 AND storefront = ?2 AND product_id = ?3",
                params![account_id, storefront as i32, product_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(Error::from)
    }

    /// Records a stat's new value from local play and marks it changed, so that no refresh
    /// overwrites it before a sync uploads it. Returns whether it was stored: `false` when the
    /// game's stats were never fetched, the key is unknown, or `value` is not of the stat's kind.
    ///
    /// # Errors
    ///
    /// The database refusing the write.
    #[inline]
    pub fn set_local_stat(
        &self,
        account_id: &str,
        storefront: Storefront,
        product_id: &str,
        stat_key: &str,
        value: StatValue,
    ) -> Result<bool> {
        let kind: Option<StatKind> = self
            .conn
            .query_row(
                "SELECT kind FROM stat_definitions
                 WHERE account_id = ?1 AND storefront = ?2 AND product_id = ?3 AND stat_key = ?4",
                params![account_id, storefront as i32, product_id, stat_key],
                |row| row.get(0),
            )
            .optional()?;
        if !kind.is_some_and(|stored| stored.holds(value)) {
            tracing::warn!(
                account_id,
                ?storefront,
                product_id,
                stat_key,
                ?kind,
                "local stat update refused: unknown stat or a value of another kind"
            );
            return Ok(false);
        }
        self.conn.execute(
            "INSERT INTO user_stats (account_id, storefront, product_id, stat_key, value, changed, changed_at)
             VALUES (?1, ?2, ?3, ?4, ?5, 1, strftime('%s','now'))
             ON CONFLICT(account_id, storefront, product_id, stat_key) DO UPDATE SET
                value = excluded.value,
                changed = 1,
                changed_at = excluded.changed_at",
            params![account_id, storefront as i32, product_id, stat_key, value],
        )?;
        tracing::debug!(
            account_id,
            ?storefront,
            product_id,
            stat_key,
            ?value,
            "recorded a local stat value"
        );
        Ok(true)
    }
}
