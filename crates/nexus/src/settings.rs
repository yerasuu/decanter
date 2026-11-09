// SPDX-License-Identifier: GPL-3.0-or-later
//! Which Nexus game a library game is, chosen by the user once and kept in `<mods>/nexus.toml`
//! beside the game's mods, so it moves and goes with them; nothing in the database.

use std::fs;
use std::io::{self, Read as _};
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::id::GameDomain;
use crate::{Error, Result};

const FILE: &str = "nexus.toml";
const SCHEMA: u32 = 1;
const MAX_BYTES: u64 = 64 << 10;

#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct GameSettings {
    pub domain: GameDomain,
}

#[derive(Serialize, Deserialize)]
struct Raw {
    schema: u32,
    domain: String,
}

impl GameSettings {
    #[must_use]
    #[inline]
    pub const fn new(domain: GameDomain) -> Self {
        Self { domain }
    }

    /// The game's choice, `None` before the user made one.
    ///
    /// # Errors
    ///
    /// [`Error::Settings`] when the file does not hold up, [`Error::Io`] when it can't be read.
    #[inline]
    pub fn read(mods: &decanter_mods::GameMods) -> Result<Option<Self>> {
        let path = path(mods);
        let file = match fs::File::open(&path) {
            Ok(file) => file,
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(err) => return Err(Error::io(&path)(err)),
        };
        let bad = |reason: &dyn core::fmt::Display| Error::Settings {
            path: path.clone(),
            reason: reason.to_string(),
        };
        let mut text = String::new();
        let read = file.take(MAX_BYTES.saturating_add(1)).read_to_string(&mut text);
        match read {
            Ok(count) if u64::try_from(count).unwrap_or(u64::MAX) > MAX_BYTES => {
                return Err(bad(&"larger than decanter writes"));
            }
            Ok(_) => {}
            Err(err) if err.kind() == io::ErrorKind::InvalidData => return Err(bad(&"not UTF-8")),
            Err(err) => return Err(Error::io(&path)(err)),
        }
        let raw: Raw = toml::from_str(decanter_core::bom::strip_str(&text, &path)).map_err(|err| bad(&err))?;
        if raw.schema != SCHEMA {
            return Err(bad(&"unknown schema"));
        }
        Ok(Some(Self {
            domain: raw.domain.parse().map_err(|err| bad(&err))?,
        }))
    }

    /// # Errors
    ///
    /// [`Error::Io`] when the file can't be written; it is replaced in one rename.
    #[inline]
    pub fn write(&self, mods: &decanter_mods::GameMods) -> Result<()> {
        let path = path(mods);
        let raw = Raw {
            schema: SCHEMA,
            domain: self.domain.as_str().to_owned(),
        };
        let text = toml::to_string(&raw).map_err(|err| Error::Settings {
            path: path.clone(),
            reason: err.to_string(),
        })?;
        fs::create_dir_all(mods.dir()).map_err(Error::io(mods.dir()))?;
        let partial = path.with_extension(format!("toml.{}.tmp", std::process::id()));
        fs::write(&partial, text).map_err(Error::io(&partial))?;
        fs::rename(&partial, &path).map_err(|err| {
            if let Err(cleanup) = fs::remove_file(&partial) {
                tracing::warn!(path = %partial.display(), %cleanup, "could not remove a partial file");
            }
            Error::io(&path)(err)
        })?;
        tracing::info!(domain = %self.domain, "nexus game settings saved");
        Ok(())
    }
}

fn path(mods: &decanter_mods::GameMods) -> PathBuf {
    mods.dir().join(FILE)
}
