//! The local user a game window plays as (`--user-id`), for co-op with two windows on one PC: each
//! window then touches only the saves of its own user.

use super::LauncherSettings;

/// Key under `titles.<TITLE_ID>` in the launch settings.
pub const KEY: &str = "local_player";

const MAX_DIGITS: usize = 10;

#[derive(Debug, thiserror::Error)]
#[error("local player {0:?} is not 1 to {MAX_DIGITS} digits")]
pub struct LocalPlayerError(String);

impl LocalPlayerError {
    pub fn value(&self) -> &str {
        &self.0
    }
}

/// Digits only, as psport's `--user-id` takes them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LocalPlayer(String);

impl LocalPlayer {
    /// Empty is no local player, the default; anything else that isn't 1 to 10 ASCII digits is
    /// refused as typed rather than trimmed or cut.
    pub fn parse(value: &str) -> Result<Option<Self>, LocalPlayerError> {
        if value.is_empty() {
            return Ok(None);
        }
        if value.len() <= MAX_DIGITS && value.bytes().all(|byte| byte.is_ascii_digit()) {
            Ok(Some(Self(value.to_owned())))
        } else {
            Err(LocalPlayerError(value.escape_default().to_string()))
        }
    }

    /// `title_id`'s saved value, checked again since the file can be edited by hand.
    pub fn of_title(settings: &LauncherSettings, title_id: &str) -> Result<Option<Self>, LocalPlayerError> {
        Self::parse(settings.title_text(title_id, KEY).unwrap_or_default())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}
