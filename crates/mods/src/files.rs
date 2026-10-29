//! The TOML files decanter keeps beside the mods, read with a size cap and replaced in one rename
//! so a crash mid-write never leaves half a file behind.

use std::fs;
use std::io::{self, Read as _};
use std::path::Path;

use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::{Error, Result};

/// A `mod.toml` listing 200 000 files stays under this; anything larger was not written by decanter.
const MAX_TOML_BYTES: u64 = 64 << 20;

/// `None` when the file does not exist.
pub(crate) fn read<T>(path: &Path) -> Result<Option<T>>
where
    T: DeserializeOwned,
{
    let file = match fs::File::open(path) {
        Ok(file) => file,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(Error::io(path)(err)),
    };
    let mut text = String::new();
    let read = file.take(MAX_TOML_BYTES.saturating_add(1)).read_to_string(&mut text);
    match read {
        Ok(count) if u64::try_from(count).unwrap_or(u64::MAX) > MAX_TOML_BYTES => {
            Err(Error::malformed(path, &"larger than decanter writes"))
        }
        Ok(_) => toml::from_str(&text)
            .map(Some)
            .map_err(|err| Error::malformed(path, &err)),
        Err(err) if err.kind() == io::ErrorKind::InvalidData => Err(Error::malformed(path, &"not UTF-8")),
        Err(err) => Err(Error::io(path)(err)),
    }
}

pub(crate) fn write<T>(path: &Path, value: &T) -> Result<()>
where
    T: Serialize,
{
    let text = toml::to_string(value).map_err(|err| Error::malformed(path, &err))?;
    let partial = path.with_extension(format!("toml.{}.tmp", std::process::id()));
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(Error::io(parent))?;
    }
    fs::write(&partial, text).map_err(Error::io(&partial))?;
    fs::rename(&partial, path).map_err(|err| {
        if let Err(cleanup) = fs::remove_file(&partial) {
            tracing::warn!(path = %partial.display(), %cleanup, "could not remove a partial file");
        }
        Error::io(path)(err)
    })
}
