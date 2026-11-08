// SPDX-License-Identifier: GPL-3.0-or-later
//! The signed body: `Manifest { repeated Package packages = 1 }`, `Package { string name = 1;
//! repeated File files = 2; repeated Dir dirs = 3 }`, `File { string path = 1; uint32 mode = 2;
//! int64 size = 3; string created = 4; Hash hash = 5; bool hidden = 6; bool system = 7 }`,
//! `Dir { string path = 1; uint32 mode = 2 }` and `Hash { HashAlgorithm algorithm = 1; bytes
//! value = 2 }` with `sha256 = 0, shake128 = 1` (nile's descriptor). Counts are capped as entries
//! are read, so a body of millions of empty entries stops at the cap instead of filling memory.

use std::collections::HashSet;

use decanter_runtime::install::{ManifestPath, path};

use super::wire::Fields;
use super::{MAX_DIRS, MAX_FILES, Manifest, ManifestError, ManifestFile, Sha256Digest, shown};

pub(super) fn decode(body: &[u8]) -> Result<Manifest, ManifestError> {
    let mut package = None;
    let mut fields = Fields::new(body);
    while let Some((number, value)) = fields.next_field()? {
        if number == 1 {
            if package.is_some() {
                return Err(ManifestError::SeveralPackages);
            }
            package = Some(value.bytes("package")?);
        }
    }
    let manifest = decode_package(package.ok_or(ManifestError::NoPackage)?)?;
    check_layout(&manifest)?;
    Ok(manifest)
}

/// The package's `name` is skipped: nothing nile does with a manifest reads it.
fn decode_package(bytes: &[u8]) -> Result<Manifest, ManifestError> {
    let mut manifest = Manifest {
        files: Vec::new(),
        dirs: Vec::new(),
        total_size: 0,
    };
    let mut fields = Fields::new(bytes);
    while let Some((number, value)) = fields.next_field()? {
        match number {
            2 => {
                if manifest.files.len() >= MAX_FILES {
                    return Err(ManifestError::TooMany {
                        what: "files",
                        limit: MAX_FILES,
                    });
                }
                let file = decode_file(value.bytes("file")?)?;
                manifest.total_size = manifest
                    .total_size
                    .checked_add(file.size)
                    .ok_or(ManifestError::TotalSizeOverflow)?;
                manifest.files.push(file);
            }
            3 => {
                if manifest.dirs.len() >= MAX_DIRS {
                    return Err(ManifestError::TooMany {
                        what: "directories",
                        limit: MAX_DIRS,
                    });
                }
                if let Some(dir) = decode_dir(value.bytes("directory")?)? {
                    manifest.dirs.push(dir);
                }
            }
            _ => {}
        }
    }
    Ok(manifest)
}

fn decode_file(bytes: &[u8]) -> Result<ManifestFile, ManifestError> {
    let mut raw_path = "";
    let mut size = 0;
    let mut hash_field = None;
    let mut fields = Fields::new(bytes);
    while let Some((number, value)) = fields.next_field()? {
        match number {
            1 => raw_path = value.string("file path")?,
            3 => size = value.varint("file size")?,
            5 => hash_field = Some(value.bytes("file hash")?),
            _ => {}
        }
    }
    let path = parse_path(raw_path)?;
    // An int64 travels as its two's complement in a varint, so a negative size has the top bit set.
    if i64::try_from(size).is_err() {
        return Err(ManifestError::NegativeSize { path: shown(raw_path) });
    }
    let hash = hash_field.ok_or_else(|| ManifestError::MissingHash { path: shown(raw_path) })?;
    Ok(ManifestFile {
        sha256: decode_hash(hash, raw_path)?,
        path,
        size,
    })
}

/// proto2 gives an absent algorithm its first value, `sha256`.
fn decode_hash(bytes: &[u8], raw_path: &str) -> Result<Sha256Digest, ManifestError> {
    let mut algorithm = 0;
    let mut value_field: &[u8] = &[];
    let mut fields = Fields::new(bytes);
    while let Some((number, value)) = fields.next_field()? {
        match number {
            1 => algorithm = value.varint("hash algorithm")?,
            2 => value_field = value.bytes("hash value")?,
            _ => {}
        }
    }
    if algorithm != 0 {
        return Err(ManifestError::UnsupportedHash {
            path: shown(raw_path),
            algorithm,
        });
    }
    let digest = <[u8; 32]>::try_from(value_field).map_err(|_wrong_len| ManifestError::BadHashLength {
        path: shown(raw_path),
        len: value_field.len(),
    })?;
    Ok(Sha256Digest(digest))
}

/// Amazon lists the install root as a directory with no path; it names nothing to create.
fn decode_dir(bytes: &[u8]) -> Result<Option<ManifestPath>, ManifestError> {
    let mut raw_path = "";
    let mut fields = Fields::new(bytes);
    while let Some((number, value)) = fields.next_field()? {
        if number == 1 {
            raw_path = value.string("directory path")?;
        }
    }
    if raw_path.is_empty() {
        return Ok(None);
    }
    parse_path(raw_path).map(Some)
}

fn parse_path(raw: &str) -> Result<ManifestPath, ManifestError> {
    ManifestPath::parse(raw).map_err(|fault| ManifestError::InvalidPath {
        path: shown(raw),
        fault,
    })
}

/// Paths are compared ignoring case, since the game opens them through Wine, which does: two files
/// that differ in case only would be one file to the game, and a file that is also another entry's
/// directory could not be created at all.
fn check_layout(manifest: &Manifest) -> Result<(), ManifestError> {
    let mut files = HashSet::with_capacity(manifest.files.len());
    for file in &manifest.files {
        if !files.insert(file.path.folded()) {
            return Err(ManifestError::DuplicatePath {
                path: shown(file.path.as_str()),
            });
        }
    }
    let conflict = |entry: &ManifestPath| ManifestError::PathConflict {
        path: shown(entry.as_str()),
    };
    for file in &manifest.files {
        if path::ancestors(&file.path.folded()).any(|dir| files.contains(dir)) {
            return Err(conflict(&file.path));
        }
    }
    for dir in &manifest.dirs {
        let folded = dir.folded();
        if files.contains(&folded) || path::ancestors(&folded).any(|parent| files.contains(parent)) {
            return Err(conflict(dir));
        }
    }
    Ok(())
}
