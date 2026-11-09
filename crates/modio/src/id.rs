// SPDX-License-Identifier: GPL-3.0-or-later
//! What mod.io's answers carry, checked once where it enters: numeric ids, URLs, display text,
//! hashes and timestamps.
//!
//! The hosts are the ones the REST API reference names: `api.mod.io` (and the per-game
//! `g-<id>.modapi.io`) for the API, `mod.io` for the pages and terms, `*.modcdn.io` for images and
//! files. Anything else is refused, as an address in an answer and as a redirect.

use core::fmt;
use core::str::FromStr;

use crate::{Error, Result};

const MAX_URL: usize = 4096;
/// Twenty digits hold any `u64`; mod.io's ids are far shorter.
const MAX_ID_DIGITS: usize = 20;
const DOMAINS: [&str; 3] = ["mod.io", "modapi.io", "modcdn.io"];

macro_rules! decanter_modio_id {
    ($(#[$doc:meta])* $name:ident) => {
        $(#[$doc])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name(u64);

        impl $name {
            /// `None` for 0, which mod.io never gives an object.
            #[must_use]
            #[inline]
            pub const fn new(value: u64) -> Option<Self> {
                if value == 0 { None } else { Some(Self(value)) }
            }

            #[must_use]
            #[inline]
            pub const fn get(self) -> u64 {
                self.0
            }
        }

        impl FromStr for $name {
            type Err = Error;

            #[inline]
            fn from_str(text: &str) -> Result<Self> {
                let invalid = || Error::InvalidId(text.to_owned());
                if text.is_empty() || text.len() > MAX_ID_DIGITS || !text.bytes().all(|byte| byte.is_ascii_digit()) {
                    return Err(invalid());
                }
                text.parse::<u64>().ok().and_then(Self::new).ok_or_else(invalid)
            }
        }

        impl fmt::Display for $name {
            #[inline]
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{}", self.0)
            }
        }
    };
}

decanter_modio_id!(
    /// A game on mod.io.
    GameId
);
decanter_modio_id!(
    /// A mod on mod.io, apart from decanter-mods' own `ModId`.
    ModioModId
);
decanter_modio_id!(
    /// One file (modfile) of a mod.
    FileId
);

/// Whether `host` is one of mod.io's domains or a subdomain of one.
pub(crate) fn is_modio_host(host: &str) -> bool {
    DOMAINS.iter().any(|domain| {
        host.eq_ignore_ascii_case(domain)
            || host
                .len()
                .checked_sub(domain.len().saturating_add(1))
                .and_then(|cut| host.get(cut..))
                .is_some_and(|tail| tail.eq_ignore_ascii_case(&format!(".{domain}")))
    })
}

/// `text` as a URL decanter may ask or show: HTTPS on a mod.io host, on its default port, without
/// credentials in it.
pub(crate) fn checked_url(text: &str) -> Option<url::Url> {
    if text.len() > MAX_URL {
        return None;
    }
    let url = url::Url::parse(text).ok()?;
    let is_allowed = url.scheme() == "https"
        && url.username().is_empty()
        && url.password().is_none()
        && url.port().is_none()
        && url.host_str().is_some_and(is_modio_host);
    is_allowed.then_some(url)
}

/// Text from an answer shown as it is: control characters become spaces and it is cut at
/// `max_chars`, since it is rendered as plain text and never interpreted.
pub(crate) fn display_text(text: &str, max_chars: usize) -> String {
    text.trim()
        .chars()
        .take(max_chars)
        .map(|character| if character.is_control() { ' ' } else { character })
        .collect()
}

/// `text` cut down to what an origin token holds: ASCII letters, digits, `.`, `_` and `-`, the
/// rest dropped, at most `max` bytes.
pub(crate) fn origin_token(text: &str, max: usize) -> String {
    text.chars()
        .map(|character| if character == ' ' { '_' } else { character })
        .filter(|character| character.is_ascii_alphanumeric() || matches!(character, '.' | '_' | '-'))
        .take(max)
        .collect()
}

/// A 32-digit hex MD5, lowercased; anything else is no hash at all.
pub(crate) fn md5_hex(text: &str) -> Option<String> {
    (text.len() == 32 && text.bytes().all(|byte| byte.is_ascii_hexdigit())).then(|| text.to_ascii_lowercase())
}

/// The UTC date of a Unix timestamp as `YYYY-MM-DD`, the only part shown; `None` for 0, which
/// mod.io uses for "never".
#[expect(
    clippy::arithmetic_side_effects,
    clippy::integer_division,
    clippy::integer_division_remainder_used,
    reason = "a positive i64 of seconds divided down to days stays far inside i64 in every step"
)]
pub(crate) fn date(unix: i64) -> Option<String> {
    if unix <= 0 {
        return None;
    }
    // Days to civil date, after Howard Hinnant's `civil_from_days`, so that no date crate is added.
    let days = unix.div_euclid(86_400).checked_add(719_468)?;
    let era = days.div_euclid(146_097);
    let day_of_era = days.rem_euclid(146_097);
    let year_of_era = (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let shifted_month = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * shifted_month + 2) / 5 + 1;
    let month = if shifted_month < 10 {
        shifted_month + 3
    } else {
        shifted_month - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    Some(format!("{year:04}-{month:02}-{day:02}"))
}

/// Seconds since the Unix epoch now, 0 if the clock is before it.
pub(crate) fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|elapsed| i64::try_from(elapsed.as_secs()).ok())
        .unwrap_or_default()
}
