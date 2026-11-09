//! `profiles/<account>.toml`: which of a game's mods one account plays with, in load order.
//! Per account rather than per game because the play tree holds what the game writes, which like
//! the prefix belongs to one account (design, open question 1).

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::path::{ModId, ModPath};
use crate::{Error, MAX_MOD_FILES, Result, files};

const SCHEMA: u32 = 1;
const MAX_BUILD_ID_CHARS: usize = 200;

#[derive(Debug, Default)]
pub(crate) struct ModSet {
    /// Enabled, in load order: the last one wins a path.
    pub(crate) mods: Vec<ModId>,
    /// Known to this account but off.
    pub(crate) disabled: Vec<ModId>,
    pub(crate) base: Base,
    /// Enabled mods left out of the play tree, with the paths an update changed under them, until
    /// the user keeps or disables them.
    pub(crate) attention: BTreeMap<ModId, Vec<ModPath>>,
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

/// Which way a mod moves in the load order, where later wins.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Shift {
    /// Loses to one more mod.
    Earlier,
    /// Wins over one more mod.
    Later,
}

#[derive(Serialize, Deserialize)]
struct RawSet {
    schema: u32,
    #[serde(default)]
    mods: Vec<String>,
    #[serde(default)]
    disabled: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    base: Option<RawBase>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    attention: BTreeMap<String, Vec<String>>,
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
        if raw.schema != SCHEMA {
            return Err(Error::malformed(path, &"unknown schema"));
        }
        if raw.mods.len().saturating_add(raw.disabled.len()) > MAX_MOD_FILES {
            return Err(Error::malformed(path, &"lists more mods than decanter writes"));
        }
        let mut set = Self::default();
        for (list, out) in [(raw.mods, &mut set.mods), (raw.disabled, &mut set.disabled)] {
            for text in list {
                let id = ModId::parse(&text).map_err(|err| Error::malformed(path, &err))?;
                out.push(id);
            }
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
        set.attention.retain(|id, _| set.mods.contains(id));
        Ok(set)
    }

    pub(crate) fn write(&self, path: &Path) -> Result<()> {
        let raw = RawSet {
            schema: SCHEMA,
            mods: self.mods.iter().map(|id| id.as_str().to_owned()).collect(),
            disabled: self.disabled.iter().map(|id| id.as_str().to_owned()).collect(),
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

    /// Enabled ones go last, so a mod turned on wins over those already on.
    pub(crate) fn set_enabled(&mut self, id: &ModId, enabled: bool) {
        self.forget(id);
        if enabled {
            self.mods.push(id.clone());
        } else {
            self.disabled.push(id.clone());
        }
    }

    pub(crate) fn forget(&mut self, id: &ModId) {
        self.mods.retain(|known| known != id);
        self.disabled.retain(|known| known != id);
        self.attention.remove(id);
    }

    /// Swaps an enabled mod with its neighbour in the load order; `false` when it is not enabled
    /// or already at that end.
    pub(crate) fn shift(&mut self, id: &ModId, step: Shift) -> bool {
        let Some(index) = self.mods.iter().position(|known| known == id) else {
            return false;
        };
        let other = match step {
            Shift::Earlier => index.checked_sub(1),
            Shift::Later => index.checked_add(1).filter(|&next| next < self.mods.len()),
        };
        let Some(other) = other else {
            return false;
        };
        self.mods.swap(index, other);
        true
    }

    pub(crate) fn is_enabled(&self, id: &ModId) -> bool {
        self.mods.contains(id)
    }

    /// A hand-edited file may list a mod twice: the first mention counts, enabled before disabled.
    fn dedup(&mut self) {
        let mut seen = std::collections::HashSet::new();
        self.mods.retain(|id| seen.insert(id.clone()));
        self.disabled.retain(|id| seen.insert(id.clone()));
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
