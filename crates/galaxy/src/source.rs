// SPDX-License-Identifier: GPL-3.0-or-later
use core::error::Error;
use core::fmt;
use core::num::NonZeroU32;

/// A game's Galaxy client id: the key the SDK signs in with and the service looks games up by.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
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

impl fmt::Display for ClientId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
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

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum SourceError {
    /// Nothing to serve for a reason the user can act on (the list was never fetched, the game is
    /// not one decanter tracks); the SDK gets `UNAVAILABLE` with the reason.
    #[error("{0}")]
    Unavailable(String),
    /// The store failed; the SDK gets `INTERNAL_ERROR` and the connection keeps serving.
    #[error(transparent)]
    Failed(Box<dyn Error + Send + Sync>),
}

/// Where the service reads a game's achievements from: decanter's database in the app, an
/// in-memory list in the test client.
pub trait AchievementSource: Send + Sync {
    /// Runs on tokio's blocking pool, so an implementation may query `SQLite` directly. A game
    /// whose list was never fetched answers an empty list (plan 3.1), not an error.
    ///
    /// # Errors
    ///
    /// [`SourceError`] as its variants describe.
    fn achievements(&self, client_id: &ClientId) -> Result<AchievementList, SourceError>;
}
