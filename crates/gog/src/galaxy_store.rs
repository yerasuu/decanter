//! The local Galaxy service's [`GameStore`] over decanter's database: what a game's SDK reads and
//! writes through `decanter-galaxy`, answered from the local tables only. Nothing here reaches
//! GOG (docs/galaxy-local-service-plan.md, sections 3.2 and 5, requirement 9); syncing is
//! `poll_achievements` and `seed_user_stats`, which never overwrite what this records.

use core::num::{NonZeroI64, NonZeroU32};
use std::path::PathBuf;
use std::time::Instant;

use decanter_core::{CachedAchievement, Db, Storefront, UserStat};
use decanter_galaxy::{Achievement, AchievementList, ClientId, GameStore, SourceError, Stat, StatKind, StatValue};

/// One signed-in game of one account. Opens the database per call, as the app's views do, so
/// that it needs no lock and never holds the write lock between requests.
pub struct GalaxyStore {
    db_path: PathBuf,
    account_id: String,
    client_id: ClientId,
    product_id: String,
    /// When the game was launched, for the running session's share of the time played.
    session_started: Instant,
}

impl GalaxyStore {
    /// `product_id` must be the GOG product the launch runs, whose build has `client_id`.
    #[must_use]
    #[inline]
    pub fn new(db_path: PathBuf, account_id: String, client_id: ClientId, product_id: String) -> Self {
        Self {
            db_path,
            account_id,
            client_id,
            product_id,
            session_started: Instant::now(),
        }
    }

    fn open(&self, client_id: &ClientId) -> Result<Db, SourceError> {
        if *client_id != self.client_id {
            return Err(SourceError::Unavailable(format!(
                "client {client_id} is not the game decanter launched"
            )));
        }
        Db::open(&self.db_path).map_err(failed)
    }

    fn cached(&self, db: &Db, achievement_id: u64) -> Result<CachedAchievement, SourceError> {
        let not_found = || SourceError::NotFound(format!("achievement {achievement_id} is not one of this game's"));
        let wanted = i64::try_from(achievement_id).map_err(|_out_of_range| not_found())?;
        db.list_achievements(&self.account_id, Storefront::Gog, &self.product_id)
            .map_err(failed)?
            .into_iter()
            .find(|cached| cached.achievement_id == Some(wanted))
            .ok_or_else(not_found)
    }
}

impl GameStore for GalaxyStore {
    #[inline]
    fn achievements(&self, client_id: &ClientId) -> Result<AchievementList, SourceError> {
        let db = self.open(client_id)?;
        let cached = db
            .list_achievements(&self.account_id, Storefront::Gog, &self.product_id)
            .map_err(failed)?;
        let mode = cached.iter().find_map(|achievement| achievement.mode.clone());
        Ok(AchievementList {
            achievements: cached.into_iter().map(to_galaxy).collect(),
            // decanter does not record which language GOG answered the list in.
            language: None,
            mode,
        })
    }

    #[inline]
    fn unlock_achievement(
        &self,
        client_id: &ClientId,
        achievement_id: u64,
        unlocked_at: NonZeroU32,
    ) -> Result<(), SourceError> {
        let db = self.open(client_id)?;
        let cached = self.cached(&db, achievement_id)?;
        if cached.unlocked {
            return Ok(());
        }
        // Every u32 of seconds is a valid time, so the default is never taken.
        let at = chrono::DateTime::from_timestamp(NonZeroI64::from(unlocked_at).get(), 0)
            .unwrap_or_default()
            .to_rfc3339();
        db.mark_local_unlock(
            &self.account_id,
            Storefront::Gog,
            &self.product_id,
            &cached.achievement_key,
            &at,
        )
        .map_err(failed)?;
        Ok(())
    }

    #[inline]
    fn clear_achievement(&self, client_id: &ClientId, achievement_id: u64) -> Result<(), SourceError> {
        let db = self.open(client_id)?;
        let cached = self.cached(&db, achievement_id)?;
        db.clear_local_unlock(
            &self.account_id,
            Storefront::Gog,
            &self.product_id,
            &cached.achievement_key,
        )
        .map_err(failed)?;
        Ok(())
    }

    #[inline]
    fn user_stats(&self, client_id: &ClientId) -> Result<Vec<Stat>, SourceError> {
        let db = self.open(client_id)?;
        let fetched = db
            .user_stats_fetched_at(&self.account_id, Storefront::Gog, &self.product_id)
            .map_err(failed)?;
        if fetched.is_none() {
            return Err(SourceError::Unavailable(
                "this game's stats were never fetched from GOG".to_owned(),
            ));
        }
        let stored = db
            .list_user_stats(&self.account_id, Storefront::Gog, &self.product_id)
            .map_err(failed)?;
        let total = stored.len();
        let stats: Vec<_> = stored.into_iter().filter_map(to_galaxy_stat).collect();
        if stats.len() < total {
            tracing::warn!(
                account_id = %self.account_id,
                product_id = %self.product_id,
                skipped = total.saturating_sub(stats.len()),
                "stats with a non-numeric GOG id left out"
            );
        }
        Ok(stats)
    }

    #[inline]
    fn set_stat(&self, client_id: &ClientId, stat_key: &str, value: StatValue) -> Result<(), SourceError> {
        let db = self.open(client_id)?;
        let stored_value = match value {
            StatValue::Int(int) => decanter_core::StatValue::Int(int),
            StatValue::Float(float) => decanter_core::StatValue::Float(f64::from(float)),
        };
        let stored = db
            .set_local_stat(
                &self.account_id,
                Storefront::Gog,
                &self.product_id,
                stat_key,
                stored_value,
            )
            .map_err(failed)?;
        if stored {
            Ok(())
        } else {
            Err(SourceError::NotFound(format!(
                "stat {stat_key} is not one of this game's"
            )))
        }
    }

    #[inline]
    fn seconds_played(&self, client_id: &ClientId) -> Result<u64, SourceError> {
        let db = self.open(client_id)?;
        let seconds_total = db
            .get_playtime(&self.account_id, Storefront::Gog, &self.product_id)
            .map_err(failed)?
            .seconds_total;
        // `add_playtime` credits a session only when it ends, so the running one is added here.
        let stored = u64::try_from(seconds_total).unwrap_or(0);
        Ok(stored.saturating_add(self.session_started.elapsed().as_secs()))
    }
}

fn failed(err: decanter_core::Error) -> SourceError {
    SourceError::Failed(Box::new(err))
}

fn to_galaxy(cached: CachedAchievement) -> Achievement {
    let unlocked_at = cached.unlocked.then(|| {
        cached
            .unlocked_at
            .as_deref()
            .and_then(unix_seconds)
            // The wire's `0` means locked, so an unlock whose stored time cannot be read is
            // served at the epoch's first second rather than as locked.
            .unwrap_or(NonZeroU32::MIN)
    });
    Achievement {
        achievement_id: cached.achievement_id.and_then(|id| u64::try_from(id).ok()),
        key: cached.achievement_key,
        name: cached.name,
        description: cached.description,
        image_url_locked: cached.icon_locked_url,
        image_url_unlocked: cached.icon_unlocked_url,
        visible_while_locked: cached.visible_while_locked.unwrap_or(true),
        unlocked_at,
        rarity: cached.rarity.map(narrow),
        rarity_level_description: cached.rarity_level_description,
        rarity_level_slug: cached.rarity_level_slug,
    }
}

/// GOG's `2021-10-12T00:00:56+0000` or the RFC 3339 that `mark_local_unlock` callers store.
fn unix_seconds(raw: &str) -> Option<NonZeroU32> {
    let at = chrono::DateTime::parse_from_rfc3339(raw)
        .or_else(|_| chrono::DateTime::parse_from_str(raw, "%Y-%m-%dT%H:%M:%S%z"))
        .ok()?;
    u32::try_from(at.timestamp()).ok().and_then(NonZeroU32::new)
}

fn to_galaxy_stat(stored: UserStat) -> Option<Stat> {
    let definition = stored.definition;
    let stat_id = definition.stat_id.parse().ok()?;
    let value = |stored: decanter_core::StatValue| match stored {
        decanter_core::StatValue::Int(int) => StatValue::Int(int),
        decanter_core::StatValue::Float(float) => StatValue::Float(narrow(float)),
    };
    Some(Stat {
        stat_id,
        key: definition.stat_key,
        kind: match definition.kind {
            decanter_core::StatKind::Int => StatKind::Int,
            decanter_core::StatKind::Float => StatKind::Float,
            decanter_core::StatKind::AvgRate => StatKind::AvgRate,
        },
        value: value(stored.value),
        default_value: definition.default_value.map(value),
        min_value: definition.min_value.map(value),
        max_value: definition.max_value.map(value),
        max_change: definition.max_change.map(value),
        increment_only: definition.increment_only,
        window: definition.window,
    })
}

#[expect(
    clippy::cast_possible_truncation,
    clippy::as_conversions,
    reason = "the SDK's floats are f32 and the tables keep f64: the nearest f32 is the value the SDK can hold"
)]
const fn narrow(value: f64) -> f32 {
    value as f32
}
