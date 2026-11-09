//! zip through the `zip` crate, whose name handling is not relied on: every name goes through
//! [`super::entry_path`].

use std::fs;
use std::path::Path;

use zip::ZipArchive;
use zip::result::ZipError;

use super::{RawEntry, Sink};
use crate::{Error, MAX_ENTRIES, Result};

const S_IFMT: u32 = 0o170_000;
const S_IFREG: u32 = 0o100_000;
const S_IFDIR: u32 = 0o040_000;

fn open(source: &Path) -> Result<ZipArchive<fs::File>> {
    let file = fs::File::open(source).map_err(Error::io(source))?;
    ZipArchive::new(file).map_err(|err| archive_error(source, err))
}

fn archive_error(source: &Path, err: ZipError) -> Error {
    match err {
        ZipError::Io(err) => Error::io(source)(err),
        ZipError::InvalidPassword => Error::Encrypted,
        other => Error::Archive(other.to_string()),
    }
}

pub(super) fn list(source: &Path) -> Result<Vec<RawEntry>> {
    let archive = open(source)?;
    if archive.len() > MAX_ENTRIES {
        return Err(Error::TooManyEntries);
    }
    let mut entries = Vec::with_capacity(archive.len());
    for index in 0..archive.len() {
        let file = archive.by_index_data(index).map_err(|err| archive_error(source, err))?;
        let name = file.name().map_err(|err| archive_error(source, err))?.into_owned();
        if file.encrypted() {
            return Err(Error::Encrypted);
        }
        // A mode without a type (written on Windows) is a plain file; any other type than file or
        // folder is a link, device or pipe.
        let is_special = file.is_symlink()
            || file
                .unix_mode()
                .is_some_and(|mode| !matches!(mode & S_IFMT, 0 | S_IFREG | S_IFDIR));
        if is_special {
            return Err(Error::LinkEntry(name));
        }
        entries.push(RawEntry {
            is_dir: file.is_dir(),
            size: file.size(),
            packed: Some(file.compressed_size()),
            name,
        });
    }
    Ok(entries)
}

pub(super) fn extract(source: &Path, sink: &mut Sink<'_>) -> Result<()> {
    let mut archive = open(source)?;
    for index in 0..archive.len() {
        let mut file = archive.by_index(index).map_err(|err| archive_error(source, err))?;
        let name = file.name().map_err(|err| archive_error(source, err))?.into_owned();
        if !sink.wants(&name) {
            continue;
        }
        sink.write(&name, &mut file)?;
    }
    Ok(())
}
