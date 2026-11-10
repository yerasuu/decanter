// SPDX-License-Identifier: GPL-3.0-or-later
//! `packs/<pack id>.local.toml`: which mod each entry was matched to on this machine and whether
//! applying the pack installed it or adopted one already there, so *remove pack and its mods*
//! offers exactly what the pack brought (design, sections 3 and 6.6). Never exported; losing it
//! only makes every mod count as adopted, which is never deleted.

use std::collections::BTreeMap;
use std::path::Path;

use decanter_mods::ModId;
use serde::{Deserialize, Serialize};

use crate::{EntryKey, Error, Field, MAX_ENTRIES, MAX_PACK_BYTES, PackId, Reason, Result, file};

const SCHEMA: u32 = 1;

#[derive(Debug, Clone)]
pub struct LocalNotes {
    pack: PackId,
    entries: BTreeMap<EntryKey, Note>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Note {
    pub id: ModId,
    /// The pack's apply imported it; `false` for a mod that was already there.
    pub installed: bool,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawNotes {
    schema: u32,
    pack: String,
    #[serde(default, rename = "entry", skip_serializing_if = "BTreeMap::is_empty")]
    entries: BTreeMap<String, RawNote>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawNote {
    #[serde(rename = "mod")]
    id: String,
    installed: bool,
}

impl LocalNotes {
    #[must_use]
    #[inline]
    pub const fn new(pack: PackId) -> Self {
        Self {
            pack,
            entries: BTreeMap::new(),
        }
    }

    #[must_use]
    #[inline]
    pub const fn pack(&self) -> &PackId {
        &self.pack
    }

    #[must_use]
    #[inline]
    pub fn get(&self, key: &EntryKey) -> Option<&Note> {
        self.entries.get(key)
    }

    /// Once an entry has a mod: an adoption never overwrites a record that the pack installed the
    /// same mod, so re-running apply keeps what the first run brought.
    #[inline]
    pub fn record(&mut self, key: EntryKey, id: ModId, installed: bool) {
        let installed = installed
            || self
                .entries
                .get(&key)
                .is_some_and(|note| note.id == id && note.installed);
        self.entries.insert(key, Note { id, installed });
    }

    #[inline]
    pub fn forget(&mut self, key: &EntryKey) {
        self.entries.remove(key);
    }

    /// The mods applying the pack imported, the only ones *remove pack and its mods* may offer.
    #[inline]
    pub fn installed(&self) -> impl Iterator<Item = &ModId> {
        self.entries.values().filter(|note| note.installed).map(|note| &note.id)
    }

    /// `None` when the file does not exist, which counts every matched mod as adopted.
    ///
    /// # Errors
    ///
    /// [`Error::Io`] and whatever [`LocalNotes::parse`] refuses.
    #[inline]
    pub fn read(path: &Path) -> Result<Option<Self>> {
        file::read_capped(path)?.map(|bytes| Self::parse(&bytes)).transpose()
    }

    /// # Errors
    ///
    /// [`Error::Encode`] and [`Error::Io`].
    #[inline]
    pub fn write(&self, path: &Path) -> Result<()> {
        file::write_replacing(path, &self.to_toml()?)
    }

    /// # Errors
    ///
    /// [`Error::TooLarge`], [`Error::NotUtf8`], [`Error::Syntax`], [`Error::Schema`], and
    /// [`Error::Invalid`] for a pack id, entry key or mod id that does not parse.
    #[inline]
    pub fn parse(bytes: &[u8]) -> Result<Self> {
        if bytes.len() > MAX_PACK_BYTES {
            return Err(Error::TooLarge);
        }
        let text = core::str::from_utf8(bytes).map_err(|_err| Error::NotUtf8)?;
        let raw: RawNotes = toml::from_str(text).map_err(|err| Error::syntax(text, &err))?;
        if raw.schema != SCHEMA {
            return Err(Error::Schema { found: raw.schema });
        }
        let pack = raw
            .pack
            .parse()
            .map_err(|reason| Error::invalid(Field::top("pack"), reason))?;
        if raw.entries.len() > MAX_ENTRIES {
            return Err(Error::TooManyEntries);
        }
        let mut entries = BTreeMap::new();
        for (key, note) in raw.entries {
            let key = key
                .parse()
                .map_err(|reason| Error::invalid(Field::top("entry"), reason))?;
            let id = ModId::parse(&note.id).map_err(|_err| Error::invalid(Field::top("mod"), Reason::Characters))?;
            entries.insert(
                key,
                Note {
                    id,
                    installed: note.installed,
                },
            );
        }
        Ok(Self { pack, entries })
    }

    /// # Errors
    ///
    /// [`Error::Encode`].
    #[inline]
    pub fn to_toml(&self) -> Result<String> {
        let raw = RawNotes {
            schema: SCHEMA,
            pack: self.pack.as_str().to_owned(),
            entries: self
                .entries
                .iter()
                .map(|(key, note)| {
                    (
                        key.as_str().to_owned(),
                        RawNote {
                            id: note.id.as_str().to_owned(),
                            installed: note.installed,
                        },
                    )
                })
                .collect(),
        };
        toml::to_string(&raw).map_err(|err| Error::Encode(err.to_string()))
    }
}
