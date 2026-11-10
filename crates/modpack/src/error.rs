// SPDX-License-Identifier: GPL-3.0-or-later
use core::fmt;
use std::io;
use std::path::PathBuf;

use crate::{MAX_EDGES, MAX_ENTRIES, MAX_PACK_BYTES, SCHEMA};

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    #[error("{}: {source}", path.display())]
    Io { path: PathBuf, source: io::Error },
    #[error("a pack file holds at most {} KiB", MAX_PACK_BYTES >> 10)]
    TooLarge,
    #[error("the pack file is not UTF-8")]
    NotUtf8,
    /// The TOML itself, or a value of the wrong type; `line` is 1-based when the parser gave a place.
    #[error("{}{message}", line.map(|line| format!("line {line}: ")).unwrap_or_default())]
    Syntax { line: Option<usize>, message: String },
    #[error("the pack needs schema {found}; this decanter reads schema {SCHEMA}")]
    Schema { found: u32 },
    #[error("a pack holds at most {MAX_ENTRIES} entries")]
    TooManyEntries,
    #[error("a pack holds at most {MAX_EDGES} requires and after references")]
    TooManyEdges,
    /// The first field, in file order, that breaks a rule of the format.
    #[error("{field}: {reason}")]
    Invalid { field: Field, reason: Reason },
    #[error("the pack can't be written as TOML: {0}")]
    Encode(String),
}

impl Error {
    pub(crate) const fn invalid(field: Field, reason: Reason) -> Self {
        Self::Invalid { field, reason }
    }

    pub(crate) fn syntax(text: &str, err: &toml::de::Error) -> Self {
        let line = err
            .span()
            .and_then(|span| text.get(..span.start))
            .map(|before| before.matches('\n').count().saturating_add(1));
        Self::Syntax {
            line,
            message: err.message().to_owned(),
        }
    }
}

/// Where in a pack file a rule broke: a header field, or a field of the n-th `[[entry]]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct Field {
    /// 0-based position among the file's `[[entry]]` tables; `None` for the header.
    pub entry: Option<usize>,
    pub name: &'static str,
}

impl Field {
    pub(crate) const fn top(name: &'static str) -> Self {
        Self { entry: None, name }
    }

    pub(crate) const fn entry(index: usize, name: &'static str) -> Self {
        Self {
            entry: Some(index),
            name,
        }
    }
}

impl fmt::Display for Field {
    #[inline]
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.entry {
            Some(index) => write!(f, "entry {} `{}`", index.saturating_add(1), self.name),
            None => write!(f, "`{}`", self.name),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Reason {
    Missing,
    /// Present, but not one of the fields its entry's `platform` has.
    NotForPlatform,
    Empty,
    TooLong,
    /// A character the field does not allow, a control character included.
    Characters,
    /// A number outside what the field allows, zero included where an id is meant.
    OutOfRange,
    /// A `platform`, `track` or DLL mode that is not one of the known ones.
    Unknown,
    Duplicate,
    /// Not a relative path that stays inside the game.
    Path,
    /// A `requires` or `after` key that names no entry of the pack.
    UnknownReference,
    SelfReference,
    TooMany,
}

impl fmt::Display for Reason {
    #[inline]
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Missing => "missing",
            Self::NotForPlatform => "not a field of this platform",
            Self::Empty => "empty",
            Self::TooLong => "too long",
            Self::Characters => "holds a character it may not",
            Self::OutOfRange => "out of range",
            Self::Unknown => "not a known value",
            Self::Duplicate => "used twice",
            Self::Path => "not a relative path inside the game",
            Self::UnknownReference => "names no entry of the pack",
            Self::SelfReference => "names its own entry",
            Self::TooMany => "lists too many items",
        })
    }
}
