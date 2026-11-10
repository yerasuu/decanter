// SPDX-License-Identifier: GPL-3.0-or-later
//! A game's packs live beside its mods, in `<game mods dir>/packs/` (design, section 3): the recipe
//! as `<pack id>.toml` and this machine's notes as `<pack id>.local.toml`. Both are read with the
//! pack's size cap and replaced in one rename, so a crash mid-write never leaves half a file.

use std::fs;
use std::io::{self, Read as _};
use std::path::{Path, PathBuf};

use crate::{Error, MAX_PACK_BYTES, PackId, Result};

const PACKS: &str = "packs";

/// `game_dir` is the game's mods folder, `GameMods::dir`.
#[must_use]
#[inline]
pub fn pack_file(game_dir: &Path, id: &PackId) -> PathBuf {
    game_dir.join(PACKS).join(format!("{id}.toml"))
}

#[must_use]
#[inline]
pub fn local_file(game_dir: &Path, id: &PackId) -> PathBuf {
    game_dir.join(PACKS).join(format!("{id}.local.toml"))
}

/// `None` when the file does not exist.
pub(crate) fn read_capped(path: &Path) -> Result<Option<Vec<u8>>> {
    let io_error = |source| Error::Io {
        path: path.to_path_buf(),
        source,
    };
    let file = match fs::File::open(path) {
        Ok(file) => file,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(io_error(err)),
    };
    let cap = u64::try_from(MAX_PACK_BYTES).unwrap_or(u64::MAX).saturating_add(1);
    let mut bytes = Vec::new();
    file.take(cap).read_to_end(&mut bytes).map_err(io_error)?;
    if bytes.len() > MAX_PACK_BYTES {
        return Err(Error::TooLarge);
    }
    Ok(Some(bytes))
}

pub(crate) fn write_replacing(path: &Path, text: &str) -> Result<()> {
    let io_error = |at: &Path| {
        let at = at.to_path_buf();
        move |source| Error::Io { path: at, source }
    };
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(io_error(parent))?;
    }
    let partial = path.with_extension(format!("toml.{}.tmp", std::process::id()));
    fs::write(&partial, text).map_err(io_error(&partial))?;
    fs::rename(&partial, path).map_err(|err| {
        // The rename's failure is what the caller acts on; a leftover partial file only costs space
        // and carries the process id, so it can't be mistaken for a pack.
        drop(fs::remove_file(&partial));
        io_error(path)(err)
    })
}
