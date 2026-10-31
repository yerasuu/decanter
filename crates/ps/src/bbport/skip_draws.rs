//! The draws psport skips (`PS_SKIP_DRAWS`), a diagnostic chosen per game.
//!
//! `<vs>:<ps>` pairs of the shader hashes the frame capture prints for each draw, `*` for any
//! shader, and `#N` for only the N-th match of each frame, counted from 0. A port without the
//! variable ignores it.

use super::LauncherSettings;

/// Key in the title's launch settings.
pub const KEY: &str = "skip_draws";

pub const MAX_ENTRIES: usize = 64;
pub const MAX_CHARS: usize = 1024;

const MAX_HASH_DIGITS: usize = 16;

#[derive(Debug, thiserror::Error)]
pub enum SkipDrawsError {
    #[error("longer than {MAX_CHARS} characters")]
    TooLong,
    #[error("more than {MAX_ENTRIES} entries")]
    TooMany,
    #[error("entry {0:?} isn't <vs>:<ps> with an optional #N")]
    Entry(String),
}

/// A list psport's grammar accepts, entries trimmed of the whitespace around them and otherwise as
/// typed.
#[derive(Debug, PartialEq, Eq)]
pub struct SkipDraws(String);

impl SkipDraws {
    /// Empty, or only whitespace, is no draw skipped, the default.
    ///
    /// # Errors
    /// More than [`MAX_ENTRIES`] entries or [`MAX_CHARS`] characters, or the first entry psport
    /// wouldn't read, escaped.
    pub fn parse(text: &str) -> Result<Option<Self>, SkipDrawsError> {
        if text.trim().is_empty() {
            return Ok(None);
        }
        if text.split(',').nth(MAX_ENTRIES).is_some() {
            return Err(SkipDrawsError::TooMany);
        }
        let joined = text.split(',').map(str::trim).collect::<Vec<_>>().join(",");
        if joined.chars().count() > MAX_CHARS {
            return Err(SkipDrawsError::TooLong);
        }
        if let Some(bad) = joined.split(',').find(|entry| !is_entry(entry)) {
            return Err(SkipDrawsError::Entry(bad.escape_default().to_string()));
        }
        Ok(Some(Self(joined)))
    }

    /// The title's saved list, checked again since the file can be edited by hand: one psport
    /// wouldn't read is dropped and logged, so the game still starts.
    #[must_use]
    pub fn of_settings(settings: &LauncherSettings, title_id: &str) -> Option<Self> {
        let saved = settings.text(KEY)?;
        Self::parse(saved)
            .inspect_err(|err| tracing::warn!(title_id, %err, "invalid skip_draws, not passed to the port"))
            .ok()
            .flatten()
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<SkipDraws> for String {
    fn from(draws: SkipDraws) -> Self {
        draws.0
    }
}

fn is_entry(entry: &str) -> bool {
    let (pair, nth) = match entry.split_once('#') {
        Some((pair, digits)) => (pair, Some(digits)),
        None => (entry, None),
    };
    let is_nth =
        nth.is_none_or(|digits| digits.bytes().all(|byte| byte.is_ascii_digit()) && digits.parse::<u32>().is_ok());
    is_nth
        && pair
            .split_once(':')
            .is_some_and(|(vs, ps)| is_shader(vs) && is_shader(ps))
}

/// `*`, or a hash in hexadecimal with or without `0x`, of which only the digits after any leading
/// zeros count towards the 16 a 64-bit hash has.
fn is_shader(side: &str) -> bool {
    if side == "*" {
        return true;
    }
    let digits = side.strip_prefix("0x").unwrap_or(side);
    !digits.is_empty()
        && digits.bytes().all(|byte| byte.is_ascii_hexdigit())
        && digits.trim_start_matches('0').len() <= MAX_HASH_DIGITS
}
