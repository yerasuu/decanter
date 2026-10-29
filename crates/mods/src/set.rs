//! `profiles/<account>.toml`: which of a game's mods one account plays with, in load order.
//! Per account rather than per game because the play tree holds what the game writes, which like
//! the prefix belongs to one account (design, open question 1).

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::path::ModId;
use crate::{Error, MAX_MOD_FILES, Result, files};

const SCHEMA: u32 = 1;

#[derive(Debug, Default)]
pub(crate) struct ModSet {
    /// Enabled, in load order: the last one wins a path.
    pub(crate) mods: Vec<ModId>,
    /// Known to this account but off.
    pub(crate) disabled: Vec<ModId>,
}

#[derive(Serialize, Deserialize)]
struct RawSet {
    schema: u32,
    #[serde(default)]
    mods: Vec<String>,
    #[serde(default)]
    disabled: Vec<String>,
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
        set.dedup();
        Ok(set)
    }

    pub(crate) fn write(&self, path: &Path) -> Result<()> {
        let raw = RawSet {
            schema: SCHEMA,
            mods: self.mods.iter().map(|id| id.as_str().to_owned()).collect(),
            disabled: self.disabled.iter().map(|id| id.as_str().to_owned()).collect(),
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
