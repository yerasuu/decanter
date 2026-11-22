//! RAR 4 and 5 through the `unrar` crate, which builds RARLAB's UnRAR (its licence ships in
//! `LICENSES/LicenseRef-UnRAR.txt`). What that crate does not report is read from the headers
//! first ([`headers`]), and the two must agree on every file. UnRAR is given the whole destination
//! name of each file, which turns off its own path handling ("the app invoking the library cares
//! about everything including safety", `extract.cpp`), so names go through
//! [`super::entry_path`] like the other formats', and each file is decoded into one staging file
//! that is only read into the [`Sink`], never kept.

mod headers;

use std::fs;
use std::path::Path;

use unrar::error::{Code, UnrarError};
use unrar::{Archive, FileHeader, VolumeInfo};

use super::{RawEntry, Sink, check_ratio, os};
use crate::{Error, MAX_ENTRIES, Result};

/// Beside the mod's `files/` in the import's own folder, so nothing else ever writes it.
const STAGING: &str = "rar-entry";

fn archive_error(source: &Path, err: &UnrarError) -> Error {
    match err.code {
        Code::MissingPassword | Code::BadPassword => Error::Encrypted,
        Code::NoMemory => Error::Archive("UnRAR ran out of memory decoding it".to_owned()),
        Code::EOpen => Error::Archive(format!("{} can't be opened ({err})", source.display())),
        Code::Success
        | Code::EndArchive
        | Code::BadData
        | Code::BadArchive
        | Code::UnknownFormat
        | Code::ECreate
        | Code::EClose
        | Code::ERead
        | Code::EWrite
        | Code::SmallBuf
        | Code::Unknown
        | Code::EReference => Error::Archive(err.to_string()),
    }
}

fn mismatch() -> Error {
    Error::Archive("UnRAR lists other files than its headers hold".to_owned())
}

/// The name UnRAR gives a file after checking it against what its header said.
fn checked_name(listed: &FileHeader, header: Option<&headers::Header>) -> Result<String> {
    let header = header.ok_or_else(mismatch)?;
    if listed.is_encrypted() {
        return Err(Error::Encrypted);
    }
    if listed.is_split() {
        return Err(Error::MultiVolume);
    }
    if listed.is_directory() != header.is_dir || listed.unpacked_size != header.size {
        return Err(mismatch());
    }
    listed
        .filename
        .to_str()
        .map(str::to_owned)
        .ok_or_else(|| Error::InvalidPath(listed.filename.to_string_lossy().into_owned()))
}

pub(super) fn list(source: &Path) -> Result<Vec<RawEntry>> {
    let headers = headers::read(source)?;
    let archive = Archive::new(source)
        .open_for_listing()
        .map_err(|err| archive_error(source, &err))?;
    if archive.has_encrypted_headers() {
        return Err(Error::Encrypted);
    }
    if archive.volume_info() != VolumeInfo::None {
        return Err(Error::MultiVolume);
    }
    let mut expected = headers.files.iter();
    let mut entries = Vec::with_capacity(headers.files.len());
    for listed in archive {
        let listed = listed.map_err(|err| archive_error(source, &err))?;
        let header = expected.next();
        let name = checked_name(&listed, header)?;
        entries.push(RawEntry {
            name,
            is_dir: listed.is_directory(),
            size: listed.unpacked_size,
            packed: header.filter(|_| !headers.solid).map(|header| header.packed),
        });
        if entries.len() > MAX_ENTRIES {
            return Err(Error::TooManyEntries);
        }
    }
    if expected.next().is_some() {
        return Err(mismatch());
    }
    // A solid archive decodes as one stream, so its files are checked together, as a 7z block.
    if headers.solid {
        let size = headers.files.iter().map(|header| header.size).sum();
        let packed = headers.files.iter().map(|header| header.packed).sum();
        let first = entries.first().map(|entry| entry.name.as_str()).unwrap_or_default();
        check_ratio(first, size, packed)?;
    }
    Ok(entries)
}

pub(super) fn extract(source: &Path, sink: &mut Sink<'_>) -> Result<()> {
    // Read again: UnRAR is about to decode this file, not the one the preview read.
    let headers = headers::read(source)?;
    let staging = sink.files_dir.with_file_name(STAGING);
    let mut expected = headers.files.iter();
    let mut archive = Archive::new(source)
        .open_for_processing()
        .map_err(|err| archive_error(source, &err))?;
    while let Some(entry) = archive.read_header().map_err(|err| archive_error(source, &err))? {
        let listed = entry.entry();
        let name = checked_name(listed, expected.next())?;
        archive = if !listed.is_directory() && sink.wants(&name) {
            let next = entry
                .extract_to(&staging)
                .map_err(|err| extract_error(source, &staging, &err))?;
            write_staged(&staging, &name, sink)?;
            next
        } else {
            // In a solid archive this still decodes the file, which the next ones build on.
            entry.skip().map_err(|err| archive_error(source, &err))?
        };
    }
    if expected.next().is_some() {
        return Err(mismatch());
    }
    Ok(())
}

fn extract_error(source: &Path, staging: &Path, err: &UnrarError) -> Error {
    match err.code {
        Code::ECreate | Code::EWrite | Code::EClose => {
            Error::io(staging)(std::io::Error::other(format!("UnRAR: {err}")))
        }
        Code::Success
        | Code::EndArchive
        | Code::NoMemory
        | Code::BadData
        | Code::BadArchive
        | Code::UnknownFormat
        | Code::EOpen
        | Code::ERead
        | Code::SmallBuf
        | Code::Unknown
        | Code::MissingPassword
        | Code::EReference
        | Code::BadPassword => archive_error(source, err),
    }
}

/// Hands the staged file to `sink`: only a regular file of its own, since a link record the
/// headers hid would have UnRAR write a link or a copy of another file there instead.
fn write_staged(staging: &Path, name: &str, sink: &mut Sink<'_>) -> Result<()> {
    let metadata = fs::symlink_metadata(staging).map_err(Error::io(staging))?;
    if !metadata.is_file() || os::is_linked(&metadata) {
        return Err(Error::LinkEntry(name.to_owned()));
    }
    // UnRAR applies the archive's own mode or attributes, which may forbid reading or removing it.
    os::make_private(staging).map_err(Error::io(staging))?;
    let mut file = fs::File::open(staging).map_err(Error::io(staging))?;
    let written = sink.write(name, &mut file);
    drop(file);
    fs::remove_file(staging).map_err(Error::io(staging))?;
    written
}
