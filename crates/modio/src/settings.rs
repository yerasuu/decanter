// SPDX-License-Identifier: GPL-3.0-or-later
//! Which mod.io game a library game is, chosen by the user once and kept in `<mods>/modio.toml`
//! beside the game's mods, so it moves and goes with them; nothing in the database.

use std::fs;
use std::io::{self, Read as _};
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::id::{GameId, display_text};
use crate::terms::write_replacing;
use crate::{Error, Result};

const FILE: &str = "modio.toml";
const SCHEMA: u32 = 1;
const MAX_BYTES: u64 = 16 << 10;
const MAX_NAME_CHARS: usize = 200;

#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct GameSettings {
    pub game: GameId,
    /// mod.io's name of the game when it was chosen, shown until its record is read again.
    pub name: String,
}

#[derive(Serialize, Deserialize)]
struct Raw {
    schema: u32,
    game_id: u64,
    name: String,
}

impl GameSettings {
    #[must_use]
    #[inline]
    pub fn new(game: GameId, name: &str) -> Self {
        Self {
            game,
            name: display_text(name, MAX_NAME_CHARS),
        }
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
        match file.take(MAX_BYTES.saturating_add(1)).read_to_string(&mut text) {
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
        let game = GameId::new(raw.game_id).ok_or_else(|| bad(&"no game id"))?;
        Ok(Some(Self::new(game, &raw.name)))
    }

    /// # Errors
    ///
    /// [`Error::Io`] when the file can't be written; it is replaced in one rename.
    #[inline]
    pub fn write(&self, mods: &decanter_mods::GameMods) -> Result<()> {
        let path = path(mods);
        let text = toml::to_string(&Raw {
            schema: SCHEMA,
            game_id: self.game.get(),
            name: self.name.clone(),
        })
        .map_err(|err| Error::Settings {
            path: path.clone(),
            reason: err.to_string(),
        })?;
        write_replacing(mods.dir(), &path, &text)?;
        tracing::info!(game_id = %self.game, "modio game settings saved");
        Ok(())
    }
}

fn path(mods: &decanter_mods::GameMods) -> PathBuf {
    mods.dir().join(FILE)
}
