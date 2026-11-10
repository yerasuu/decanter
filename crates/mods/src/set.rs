//! `profiles/<account>.toml`: which of a game's mods one account plays with, in load order.
//! Per account rather than per game because the play tree holds what the game writes, which like
//! the prefix belongs to one account (design, open question 1).

use std::collections::{BTreeMap, HashSet};
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::path::{ModId, ModPath};
use crate::{Error, MAX_MOD_FILES, Result, files};

/// Schema 2 keeps one ordered list with an enabled flag per mod, so a mod turned off keeps its
/// slot; schema 1 (`mods` enabled in order, then `disabled`) is still read, and rewritten as 2 on
/// the next save.
const SCHEMA: u32 = 2;
const SCHEMA_SPLIT_LISTS: u32 = 1;
const MAX_BUILD_ID_CHARS: usize = 200;

#[derive(Debug, Default)]
pub(crate) struct ModSet {
    /// Every mod this account knows, in load order, on or off: the last enabled one wins a path.
    pub(crate) entries: Vec<Entry>,
    pub(crate) base: Base,
    /// Enabled mods left out of the play tree, with the paths an update changed under them, until
    /// the user keeps or disables them.
    pub(crate) attention: BTreeMap<ModId, Vec<ModPath>>,
}

#[derive(Debug, Clone)]
pub(crate) struct Entry {
    pub(crate) id: ModId,
    pub(crate) enabled: bool,
}

/// What the game's files a mod touches were when its set was last applied (design, section 3).
#[derive(Debug, Default)]
pub(crate) struct Base {
    /// The build the files were last compared at.
    pub(crate) build_id: Option<String>,
    /// Per path in the game, the install file's `sha256:<hex>`, or [`ABSENT`] for a path only a mod
    /// had.
    pub(crate) files: BTreeMap<ModPath, String>,
}

/// A path the install did not have when the mods were applied: an update that adds it is reported
/// as one that changes it.
pub(crate) const ABSENT: &str = "absent";

#[derive(Serialize, Deserialize)]
struct RawSet {
    schema: u32,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    mods: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    disabled: Vec<String>,
    #[serde(default, rename = "mod", skip_serializing_if = "Vec::is_empty")]
    entries: Vec<RawEntry>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    base: Option<RawBase>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    attention: BTreeMap<String, Vec<String>>,
}

#[derive(Serialize, Deserialize)]
struct RawEntry {
    id: String,
    enabled: bool,
}

#[derive(Serialize, Deserialize)]
struct RawBase {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    build_id: Option<String>,
    #[serde(default)]
    files: BTreeMap<String, String>,
}

impl ModSet {
    /// An account with no file yet has every mod off.
    pub(crate) fn read(path: &Path) -> Result<Self> {
        let Some(raw) = files::read::<RawSet>(path)? else {
            return Ok(Self::default());
        };
        let listed: Vec<(String, bool)> = match raw.schema {
            // Enabled first, then disabled: the enabled ones keep their order and so their winners.
            SCHEMA_SPLIT_LISTS if raw.entries.is_empty() => raw
                .mods
                .into_iter()
                .map(|id| (id, true))
                .chain(raw.disabled.into_iter().map(|id| (id, false)))
                .collect(),
            SCHEMA if raw.mods.is_empty() && raw.disabled.is_empty() => {
                raw.entries.into_iter().map(|entry| (entry.id, entry.enabled)).collect()
            }
            SCHEMA_SPLIT_LISTS | SCHEMA => return Err(Error::malformed(path, &"mixes the lists of two schemas")),
            _ => return Err(Error::malformed(path, &"unknown schema")),
        };
        if listed.len() > MAX_MOD_FILES {
            return Err(Error::malformed(path, &"lists more mods than decanter writes"));
        }
        let mut set = Self::default();
        set.entries.reserve(listed.len());
        for (text, enabled) in listed {
            let id = ModId::parse(&text).map_err(|err| Error::malformed(path, &err))?;
            set.entries.push(Entry { id, enabled });
        }
        if let Some(base) = raw.base {
            set.base = read_base(base, path)?;
        }
        if raw.attention.len() > MAX_MOD_FILES {
            return Err(Error::malformed(path, &"lists more mods than decanter writes"));
        }
        for (id, paths) in raw.attention {
            let id = ModId::parse(&id).map_err(|err| Error::malformed(path, &err))?;
            let paths = paths
                .iter()
                .map(|text| ModPath::parse(text).map_err(|err| Error::malformed(path, &err)))
                .collect::<Result<Vec<_>>>()?;
            set.attention.insert(id, paths);
        }
        set.dedup();
        let mut attention = core::mem::take(&mut set.attention);
        attention.retain(|id, _| set.is_enabled(id));
        set.attention = attention;
        Ok(set)
    }

    pub(crate) fn write(&self, path: &Path) -> Result<()> {
        let raw = RawSet {
            schema: SCHEMA,
            mods: Vec::new(),
            disabled: Vec::new(),
            entries: self
                .entries
                .iter()
                .map(|entry| RawEntry {
                    id: entry.id.as_str().to_owned(),
                    enabled: entry.enabled,
                })
                .collect(),
            base: (self.base.build_id.is_some() || !self.base.files.is_empty()).then(|| RawBase {
                build_id: self.base.build_id.clone(),
                files: self
                    .base
                    .files
                    .iter()
                    .map(|(file, hash)| (file.as_str().to_owned(), hash.clone()))
                    .collect(),
            }),
            attention: self
                .attention
                .iter()
                .map(|(id, paths)| {
                    (
                        id.as_str().to_owned(),
                        paths.iter().map(|file| file.as_str().to_owned()).collect(),
                    )
                })
                .collect(),
        };
        files::write(path, &raw)
    }

    /// The enabled mods, in load order.
    pub(crate) fn enabled(&self) -> impl Iterator<Item = &ModId> {
        self.entries.iter().filter(|entry| entry.enabled).map(|entry| &entry.id)
    }

    pub(crate) fn position(&self, id: &ModId) -> Option<usize> {
        self.entries.iter().position(|entry| entry.id == *id)
    }

    /// A mod the account already knows keeps its slot either way; a new one goes last, so it wins
    /// over those already on.
    pub(crate) fn set_enabled(&mut self, id: &ModId, enabled: bool) {
        self.attention.remove(id);
        if let Some(entry) = self.entries.iter_mut().find(|entry| entry.id == *id) {
            entry.enabled = enabled;
        } else {
            self.entries.push(Entry {
                id: id.clone(),
                enabled,
            });
        }
    }

    pub(crate) fn forget(&mut self, id: &ModId) {
        self.entries.retain(|entry| entry.id != *id);
        self.attention.remove(id);
    }

    /// Moves a known mod to slot `to`, the others keeping their relative order; a mod the account
    /// does not know yet is put there turned off. `false` when nothing moved. The caller checks
    /// `to` against [`Self::entries`]' length, one more for an unknown mod.
    pub(crate) fn place(&mut self, id: &ModId, to: usize) -> bool {
        let entry = match self.position(id) {
            Some(from) if from == to => return false,
            Some(from) => self.entries.remove(from),
            None => Entry {
                id: id.clone(),
                enabled: false,
            },
        };
        self.entries.insert(to.min(self.entries.len()), entry);
        true
    }

    /// `new` takes `old`'s slot and on or off state; whatever slot `new` had is dropped.
    pub(crate) fn replace(&mut self, old: &ModId, new: &ModId) -> bool {
        let Some(enabled) = self
            .entries
            .iter()
            .find(|entry| entry.id == *old)
            .map(|entry| entry.enabled)
        else {
            return false;
        };
        self.entries.retain(|entry| entry.id != *new);
        self.attention.remove(old);
        if let Some(entry) = self.entries.iter_mut().find(|entry| entry.id == *old) {
            *entry = Entry {
                id: new.clone(),
                enabled,
            };
        }
        true
    }

    /// As [`Self::replace`], but `old` stays right after `new`, turned off, for a rollback.
    pub(crate) fn supersede(&mut self, old: &ModId, new: &ModId) -> bool {
        self.entries.retain(|entry| entry.id != *new);
        let Some(at) = self.position(old) else {
            return false;
        };
        self.attention.remove(old);
        let Some(entry) = self.entries.get_mut(at) else {
            return false;
        };
        let enabled = core::mem::replace(&mut entry.enabled, false);
        self.entries.insert(
            at,
            Entry {
                id: new.clone(),
                enabled,
            },
        );
        true
    }

    pub(crate) fn is_enabled(&self, id: &ModId) -> bool {
        self.entries.iter().any(|entry| entry.id == *id && entry.enabled)
    }

    /// A hand-edited file may list a mod twice: the first mention counts, which in schema 1 puts
    /// enabled before disabled.
    fn dedup(&mut self) {
        let mut seen = HashSet::new();
        self.entries.retain(|entry| seen.insert(entry.id.clone()));
    }
}

fn read_base(raw: RawBase, path: &Path) -> Result<Base> {
    if raw
        .build_id
        .as_ref()
        .is_some_and(|build| build.chars().count() > MAX_BUILD_ID_CHARS || build.chars().any(char::is_control))
    {
        return Err(Error::malformed(path, &"base build_id is not one a store gives"));
    }
    if raw.files.len() > MAX_MOD_FILES {
        return Err(Error::malformed(path, &"base lists more files than decanter writes"));
    }
    let mut files = BTreeMap::new();
    for (file, hash) in raw.files {
        let is_hash = hash
            .strip_prefix("sha256:")
            .is_some_and(|hex| hex.len() == 64 && hex.bytes().all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f')));
        if !is_hash && hash != ABSENT {
            return Err(Error::malformed(path, &"a base hash is not a SHA-256"));
        }
        files.insert(ModPath::parse(&file).map_err(|err| Error::malformed(path, &err))?, hash);
    }
    Ok(Base {
        build_id: raw.build_id,
        files,
    })
}
