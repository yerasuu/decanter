// SPDX-License-Identifier: GPL-3.0-or-later
//! The names and hashes a pack carries, and the rules for its display text (design, section 8.2).

use core::fmt;
use core::str::FromStr;

use crate::Reason;

const MAX_KEY_CHARS: usize = 32;
const MAX_TEXT_CHARS: usize = 200;

/// A pack's uuid in hyphenated lowercase form, which also names its files in `packs/`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackId(String);

impl PackId {
    /// A fresh id, for a new pack and for every imported one, since an imported id is a hint and
    /// never names a file (design, section 6.7).
    #[must_use]
    #[inline]
    pub fn generate() -> Self {
        Self(uuid::Uuid::new_v4().hyphenated().to_string())
    }

    #[must_use]
    #[inline]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl FromStr for PackId {
    type Err = Reason;

    #[inline]
    fn from_str(text: &str) -> Result<Self, Reason> {
        match uuid::Uuid::try_parse(text) {
            Ok(id) if id.hyphenated().to_string() == text => Ok(Self(text.to_owned())),
            Ok(_) | Err(_) => Err(Reason::Characters),
        }
    }
}

impl fmt::Display for PackId {
    #[inline]
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// An entry's name within its pack, which `requires` and `after` refer to: `[a-z0-9_-]`, at most
/// 32 characters.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct EntryKey(String);

impl EntryKey {
    #[must_use]
    #[inline]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl FromStr for EntryKey {
    type Err = Reason;

    #[inline]
    fn from_str(text: &str) -> Result<Self, Reason> {
        if text.is_empty() {
            return Err(Reason::Empty);
        }
        if text.len() > MAX_KEY_CHARS {
            return Err(Reason::TooLong);
        }
        if !text
            .bytes()
            .all(|byte| matches!(byte, b'a'..=b'z' | b'0'..=b'9' | b'_' | b'-'))
        {
            return Err(Reason::Characters);
        }
        Ok(Self(text.to_owned()))
    }
}

impl fmt::Display for EntryKey {
    #[inline]
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// An MD5 as a provider lists it, 32 lowercase hex digits: an integrity check only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Md5(String);

impl Md5 {
    #[must_use]
    #[inline]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl FromStr for Md5 {
    type Err = Reason;

    #[inline]
    fn from_str(text: &str) -> Result<Self, Reason> {
        hex(text, 32).map(Self)
    }
}

/// The SHA-256 of a mod's archive, 64 lowercase hex digits, as `mod.toml`'s `source_sha256` keeps it.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Sha256(String);

impl Sha256 {
    #[must_use]
    #[inline]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl FromStr for Sha256 {
    type Err = Reason;

    #[inline]
    fn from_str(text: &str) -> Result<Self, Reason> {
        hex(text, 64).map(Self)
    }
}

fn hex(text: &str, digits: usize) -> Result<String, Reason> {
    if text.len() != digits {
        return Err(if text.len() > digits {
            Reason::TooLong
        } else {
            Reason::Empty
        });
    }
    if !text.bytes().all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f')) {
        return Err(Reason::Characters);
    }
    Ok(text.to_owned())
}

/// Text only ever shown: trimmed, at most 200 characters and no control character, the rule
/// `mod.toml`'s names already follow.
pub(crate) fn display_text(text: &str) -> Result<String, Reason> {
    let text = text.trim();
    if text.is_empty() {
        return Err(Reason::Empty);
    }
    if text.chars().count() > MAX_TEXT_CHARS {
        return Err(Reason::TooLong);
    }
    if text.chars().any(char::is_control) {
        return Err(Reason::Characters);
    }
    Ok(text.to_owned())
}

/// A short ASCII name whose every byte `allowed` accepts, `1..=max` bytes long.
pub(crate) fn token(text: &str, max: usize, allowed: impl Fn(u8) -> bool) -> Result<String, Reason> {
    if text.is_empty() {
        return Err(Reason::Empty);
    }
    if text.len() > max {
        return Err(Reason::TooLong);
    }
    if !text.bytes().all(allowed) {
        return Err(Reason::Characters);
    }
    Ok(text.to_owned())
}
