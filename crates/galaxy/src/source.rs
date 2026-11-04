// SPDX-License-Identifier: GPL-3.0-or-later
use core::error::Error;
use core::fmt;
use core::num::NonZeroU32;

use crate::log_id::log_id;

/// A game's Galaxy client id: the key the SDK signs in with and the service looks games up by.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct ClientId(String);

impl ClientId {
    /// GOG's client ids are decimal `u64`s (twenty digits at most); anything else is refused
    /// before it can become a lookup key.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        let valid = (1..=20).contains(&text.len()) && text.bytes().all(|byte| byte.is_ascii_digit());
        valid.then(|| Self(text.to_owned()))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// The id's [`log_id`] stand-in, so that no log line or error message carries the id itself;
/// [`ClientId::as_str`] is the id.
impl fmt::Display for ClientId {
    #[inline]
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&log_id(&self.0), f)
    }
}

impl fmt::Debug for ClientId {
    #[inline]
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ClientId({self})")
    }
}

/// One achievement as the SDK receives it (`GetUserAchievementsResponse.UserAchievement`).
#[derive(Debug, Clone)]
pub struct Achievement {
    /// GOG's numeric id, which the SDK addresses achievements by; an entry without one is not
    /// served.
    pub achievement_id: Option<u64>,
    pub key: String,
    pub name: String,
    pub description: Option<String>,
    pub image_url_locked: Option<String>,
    pub image_url_unlocked: Option<String>,
    pub visible_while_locked: bool,
    /// Unix seconds; `Some` exactly when unlocked, since the wire's `0` means locked.
    pub unlocked_at: Option<NonZeroU32>,
    pub rarity: Option<f32>,
    pub rarity_level_description: Option<String>,
    pub rarity_level_slug: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct AchievementList {
    pub achievements: Vec<Achievement>,
    /// The language the names and descriptions are in.
    pub language: Option<String>,
    /// GOG's `achievements_mode` for the game.
    pub mode: Option<String>,
}

/// The SDK's `ValueType` of a stat, as GOG defines it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatKind {
    Int,
    Float,
    AvgRate,
}

/// The SDK carries int stats as `int32` and float and average-rate stats as `float`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum StatValue {
    Int(i32),
    Float(f32),
}

impl StatValue {
    /// Exact for both kinds, so bounds of either compare without overflow or rounding.
    #[must_use]
    #[inline]
    pub fn as_f64(self) -> f64 {
        match self {
            Self::Int(value) => f64::from(value),
            Self::Float(value) => f64::from(value),
        }
    }
}

/// One stat with its stored value and the rules GOG defines for it, which the service enforces
/// on every update since nothing else does offline (plan 2.1, row 17).
#[derive(Debug, Clone)]
pub struct Stat {
    pub stat_id: u64,
    pub key: String,
    pub kind: StatKind,
    pub value: StatValue,
    pub default_value: Option<StatValue>,
    pub min_value: Option<StatValue>,
    pub max_value: Option<StatValue>,
    /// The largest step one update may take from the stored value.
    pub max_change: Option<StatValue>,
    pub increment_only: bool,
    /// Average-rate stats only: the window the SDK averages over.
    pub window: Option<f64>,
}

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum SourceError {
    /// Nothing to serve for a reason the user can act on (the list was never fetched, the game is
    /// not one decanter tracks); the SDK gets `UNAVAILABLE` with the reason.
    #[error("{0}")]
    Unavailable(String),
    /// The achievement or stat the SDK named is not in the game's stored list; the SDK gets
    /// `NOT_FOUND`.
    #[error("{0}")]
    NotFound(String),
    /// The store failed; the SDK gets `INTERNAL_ERROR` and the connection keeps serving.
    #[error(transparent)]
    Failed(Box<dyn Error + Send + Sync>),
}

/// Everything the service reads and writes for a signed-in game.
///
/// decanter's database in the app (`decanter_gog::GalaxyStore`), in-memory lists in the test
/// client: a trait so that this crate never depends on the database or on anything that reaches
/// GOG (plan 1.1, requirement 9). Every method runs on tokio's blocking pool, so an
/// implementation may query `SQLite` directly. Writes are local only and must mark what they
/// change as not yet synced, so that a refresh from GOG never lowers it (plan 3.2).
pub trait GameStore: Send + Sync {
    /// A game whose list was never fetched answers an empty list (plan 3.1), not an error.
    ///
    /// # Errors
    ///
    /// [`SourceError`] as its variants describe.
    fn achievements(&self, client_id: &ClientId) -> Result<AchievementList, SourceError>;

    /// Unlocks one achievement at `unlocked_at` (Unix seconds). One already unlocked keeps its
    /// first time and is not an error, since the SDK may store the same unlock again.
    ///
    /// # Errors
    ///
    /// [`SourceError::NotFound`] for an id the game does not have.
    fn unlock_achievement(
        &self,
        client_id: &ClientId,
        achievement_id: u64,
        unlocked_at: NonZeroU32,
    ) -> Result<(), SourceError>;

    /// Locks one achievement again, locally only: nothing clears it on GOG.
    ///
    /// # Errors
    ///
    /// [`SourceError::NotFound`] for an id the game does not have.
    fn clear_achievement(&self, client_id: &ClientId, achievement_id: u64) -> Result<(), SourceError>;

    /// [`SourceError::Unavailable`] for a game whose stats were never fetched, so that the SDK is
    /// not told it has none.
    ///
    /// # Errors
    ///
    /// [`SourceError`] as its variants describe.
    fn user_stats(&self, client_id: &ClientId) -> Result<Vec<Stat>, SourceError>;

    /// Stores a value the service has already checked against the stat's definition.
    ///
    /// # Errors
    ///
    /// [`SourceError::NotFound`] for a key the game does not have.
    fn set_stat(&self, client_id: &ClientId, stat_key: &str, value: StatValue) -> Result<(), SourceError>;

    /// decanter's own measurement: the stored total plus the running session's seconds so far.
    ///
    /// # Errors
    ///
    /// [`SourceError`] as its variants describe.
    fn seconds_played(&self, client_id: &ClientId) -> Result<u64, SourceError>;
}
