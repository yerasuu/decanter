// SPDX-License-Identifier: GPL-3.0-or-later
//! The pack (design, section 3.1) and its file. Every pack is read as untrusted input, its own
//! stored copy included, since it may have come from anyone and anything may have edited the
//! library folder since; writing re-reads what it wrote through the same checks, so an edit can
//! never store a pack that would be refused on the next read.

use std::io;
use std::path::Path;

use decanter_mods::{DllOverride, ModPath};

use crate::{EntryKey, Error, MAX_PACK_BYTES, Md5, PackId, Result, Sha256, file, raw};

#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct Pack {
    pub id: PackId,
    pub name: String,
    /// Bumped on each saved edit.
    pub revision: u32,
    pub description: Option<String>,
    pub game: GameHint,
    /// In file order, which means nothing: `order` is the position.
    pub entries: Vec<Entry>,
}

/// Which game the pack was made for, for people and for the mapping check (design, section 6.4):
/// the game it applies to is always the one it is imported into.
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct GameHint {
    pub title: Option<String>,
    /// The build the author tested; advisory, never enforced.
    pub build_hint: Option<String>,
    /// Thunderstore's community.
    pub thunderstore: Option<String>,
    /// Nexus' game domain.
    pub nexus: Option<String>,
    /// mod.io's game id.
    pub modio: Option<u64>,
}

#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct Entry {
    pub key: EntryKey,
    /// The explicit load and overwrite position, unique in the pack; gaps allowed.
    pub order: u32,
    pub source: Source,
    pub track: Track,
    pub enabled: bool,
    /// Added as a provider-declared dependency of another entry.
    pub auto: bool,
    /// The game's subfolder chosen at import; `None` for the game's root.
    pub target: Option<ModPath>,
    /// Display only; the provider's live value wins.
    pub name: Option<String>,
    /// Display only; the provider's live value wins (design, section 7).
    pub author: Option<String>,
    /// As the provider listed it; a hard check when present (design, section 6.3).
    pub md5: Option<Md5>,
    /// A hard check when present; for a `local` entry its absence means a folder, which no one else
    /// can reproduce.
    pub sha256: Option<Sha256>,
    pub size: Option<u64>,
    pub dll_overrides: Vec<DllOverride>,
    /// Hard edges: these entries are installed before this one, which stays off unless they end on.
    pub requires: Vec<EntryKey>,
    /// Ordering only: placed after these entries if they are there.
    pub after: Vec<EntryKey>,
}

/// Where an entry's file comes from, as structured ids rather than `ModOrigin`'s packed tokens, so
/// nothing in an untrusted file has to be split from the right.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Source {
    Thunderstore {
        /// `Team-Name`.
        package: String,
        /// `major.minor.patch`; with [`Track::Latest`], the one last seen.
        version: String,
    },
    Nexus {
        domain: String,
        mod_id: u64,
        /// What is fetched.
        file: u64,
        /// Display only.
        version: Option<String>,
    },
    Modio {
        game: u64,
        mod_id: u64,
        file: u64,
        version: Option<String>,
    },
    /// An archive the user gives, recognised by the entry's `sha256`, never looked for elsewhere.
    Local {
        /// The name to ask the user for; never a path.
        file_name: String,
    },
}

impl Source {
    /// The name `ModOrigin.platform` carries for it; `local` has no origin.
    #[must_use]
    #[inline]
    pub const fn platform(&self) -> &'static str {
        match self {
            Self::Thunderstore { .. } => "thunderstore",
            Self::Nexus { .. } => "nexus",
            Self::Modio { .. } => "modio",
            Self::Local { .. } => "local",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum Track {
    /// Exactly the entry's version or file, the default for every pack created and exported.
    #[default]
    Pinned,
    /// The provider's latest at apply time, recorded back when the owner's machine saves the pack.
    Latest,
}

impl Track {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Pinned => "pinned",
            Self::Latest => "latest",
        }
    }
}

impl Pack {
    /// An empty pack, revision 1, with a fresh id.
    #[must_use]
    #[inline]
    pub fn new(name: String) -> Self {
        Self {
            id: PackId::generate(),
            name,
            revision: 1,
            description: None,
            game: GameHint::default(),
            entries: Vec::new(),
        }
    }

    /// A pack file's bytes, checked against every rule of the format.
    ///
    /// # Errors
    ///
    /// [`Error::TooLarge`] past [`MAX_PACK_BYTES`], [`Error::NotUtf8`], [`Error::Syntax`] for TOML that does not
    /// parse or a value of the wrong type, [`Error::Schema`] for a schema this decanter does not
    /// read, and otherwise the first offending field as [`Error::Invalid`], [`Error::TooManyEntries`]
    /// or [`Error::TooManyEdges`].
    #[inline]
    pub fn parse(bytes: &[u8]) -> Result<Self> {
        if bytes.len() > MAX_PACK_BYTES {
            return Err(Error::TooLarge);
        }
        let text = core::str::from_utf8(bytes).map_err(|_err| Error::NotUtf8)?;
        raw::decode(text.strip_prefix('\u{feff}').unwrap_or(text))
    }

    /// A pack file someone shared: checked as [`Pack::parse`] does, and given a fresh id.
    ///
    /// # Errors
    ///
    /// As [`Pack::parse`].
    #[inline]
    pub fn import(bytes: &[u8]) -> Result<Self> {
        let mut pack = Self::parse(bytes)?;
        pack.id = PackId::generate();
        Ok(pack)
    }

    /// The pack as its file, from the typed fields only, so no field can be written that the format
    /// does not have (design, section 8.1).
    ///
    /// # Errors
    ///
    /// Whatever [`Pack::parse`] would refuse in the result, such as two entries an edit gave one
    /// `order`, or [`Error::Encode`].
    #[inline]
    pub fn to_toml(&self) -> Result<String> {
        let text = raw::encode(self)?;
        Self::parse(text.as_bytes())?;
        Ok(text)
    }

    /// A pack file, from the library or from wherever the user picked it, checked as
    /// [`Pack::parse`] does.
    ///
    /// # Errors
    ///
    /// [`Error::Io`] when it can't be read, and whatever [`Pack::parse`] refuses.
    #[inline]
    pub fn read(path: &Path) -> Result<Self> {
        file::read_capped(path)?.map_or_else(
            || {
                Err(Error::Io {
                    path: path.to_path_buf(),
                    source: io::ErrorKind::NotFound.into(),
                })
            },
            |bytes| Self::parse(&bytes),
        )
    }

    /// # Errors
    ///
    /// Whatever [`Pack::to_toml`] refuses, and [`Error::Io`].
    #[inline]
    pub fn write(&self, path: &Path) -> Result<()> {
        file::write_replacing(path, &self.to_toml()?)
    }

    #[must_use]
    #[inline]
    pub fn entry(&self, key: &EntryKey) -> Option<&Entry> {
        self.entries.iter().find(|entry| entry.key == *key)
    }
}

impl Entry {
    /// Pinned, enabled, not `auto`, for the game's root, with no hash, overrides or edges.
    #[must_use]
    #[inline]
    pub const fn new(key: EntryKey, order: u32, source: Source) -> Self {
        Self {
            key,
            order,
            source,
            track: Track::Pinned,
            enabled: true,
            auto: false,
            target: None,
            name: None,
            author: None,
            md5: None,
            sha256: None,
            size: None,
            dll_overrides: Vec::new(),
            requires: Vec::new(),
            after: Vec::new(),
        }
    }

    /// Whether someone else applying the pack can get the same files: not for a local folder,
    /// which has no archive to recognise (design, section 4).
    #[must_use]
    #[inline]
    pub const fn is_shareable(&self) -> bool {
        !matches!(self.source, Source::Local { .. }) || self.sha256.is_some()
    }
}
