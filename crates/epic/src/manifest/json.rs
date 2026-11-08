// SPDX-License-Identifier: GPL-3.0-or-later
//! The JSON manifest of older builds (spec, 6.5). Its numbers are "blobs": strings of three decimal
//! digits per byte, least-significant byte first. Its GUIDs are 32 hex digits read as four
//! big-endian words, and every chunk's window is 1 MiB. Members decanter does not read are counted
//! and logged, never an error.

use std::collections::HashMap;

use serde_json::{Map, Value};

use super::{
    Chunk, Guid, MAX_CHUNKS, MAX_FILES, MAX_PARTS, MAX_TAGS_PER_FILE, MAX_VERSION, MAX_WINDOW, ManifestError, Parsed,
    RawFile, RawPart, Sha1Digest,
};

/// `ManifestFileVersion` when a manifest leaves it out (spec, 6.5).
const DEFAULT_VERSION: u64 = 13;
/// A blob of a SHA-1 is the longest one read.
const MAX_BLOB_BYTES: usize = 20;

const READ_MEMBERS: [&str; 15] = [
    "ManifestFileVersion",
    "bIsFileData",
    "AppID",
    "AppNameString",
    "BuildVersionString",
    "LaunchExeString",
    "LaunchCommand",
    "PrereqIds",
    "PrereqName",
    "PrereqPath",
    "PrereqArgs",
    "ChunkFilesizeList",
    "ChunkHashList",
    "ChunkShaList",
    "DataGroupList",
];

pub(super) fn parse(raw: &[u8]) -> Result<Parsed, ManifestError> {
    let root: Value =
        serde_json::from_slice(raw).map_err(|_| ManifestError::Malformed("the JSON manifest does not parse"))?;
    let root = root
        .as_object()
        .ok_or(ManifestError::Malformed("the JSON manifest is not an object"))?;
    let unknown = root
        .keys()
        .filter(|key| {
            !READ_MEMBERS.contains(&key.as_str()) && !matches!(key.as_str(), "FileManifestList" | "CustomFields")
        })
        .count();
    if unknown > 0 {
        tracing::info!(unknown, "epic JSON manifest has members decanter does not read");
    }
    let version = match root.get("ManifestFileVersion") {
        Some(value) => blob_u64(value)?,
        None => DEFAULT_VERSION,
    };
    let feature_level = u32::try_from(version)
        .ok()
        .filter(|level| *level <= MAX_VERSION)
        .ok_or_else(|| ManifestError::UnsupportedVersion(u32::try_from(version).unwrap_or(u32::MAX)))?;
    if root.get("bIsFileData").and_then(Value::as_bool) == Some(true) {
        return Err(ManifestError::FileData);
    }
    let chunks = chunks(root)?;
    let files = files(root)?;
    Ok(Parsed {
        feature_level,
        app_name: text(root, "AppNameString")?,
        build_version: text(root, "BuildVersionString")?,
        launch_exe: text(root, "LaunchExeString")?,
        launch_command: text(root, "LaunchCommand")?,
        chunks,
        files,
    })
}

fn text(root: &Map<String, Value>, member: &str) -> Result<String, ManifestError> {
    match root.get(member) {
        None | Some(Value::Null) => Ok(String::new()),
        Some(Value::String(value)) => Ok(value.clone()),
        Some(_) => Err(ManifestError::Malformed(
            "a JSON manifest string member is not a string",
        )),
    }
}

/// The chunks `ChunkHashList` names, with their group, size and SHA-1 from the lists beside it.
fn chunks(root: &Map<String, Value>) -> Result<Vec<Chunk>, ManifestError> {
    let hashes =
        keyed(root, "ChunkHashList")?.ok_or(ManifestError::Malformed("the JSON manifest has no chunk list"))?;
    if hashes.len() > MAX_CHUNKS {
        return Err(ManifestError::TooMany {
            what: "chunks",
            limit: MAX_CHUNKS,
        });
    }
    let groups = keyed(root, "DataGroupList")?.unwrap_or_default();
    let shas = keyed(root, "ChunkShaList")?.unwrap_or_default();
    let mut chunks = Vec::with_capacity(hashes.len());
    for (guid, hash) in hashes {
        let group = groups
            .get(&guid)
            .ok_or(ManifestError::Malformed(
                "a chunk of the JSON manifest has no data group",
            ))
            .and_then(|value| blob_u64(value))?;
        let sha1 = shas.get(&guid).map(|value| sha_hex(value)).transpose()?;
        chunks.push(Chunk {
            guid,
            rolling_hash: blob_u64(hash)?,
            sha1,
            group: u8::try_from(group).map_err(|_| ManifestError::Malformed("a chunk's data group is past 255"))?,
            window_size: MAX_WINDOW,
            secret: Guid([0; 4]),
        });
    }
    Ok(chunks)
}

/// A member that maps chunk GUIDs to values; `None` when the manifest leaves it out.
fn keyed<'root>(
    root: &'root Map<String, Value>,
    member: &str,
) -> Result<Option<HashMap<Guid, &'root Value>>, ManifestError> {
    let Some(value) = root.get(member) else {
        return Ok(None);
    };
    let object = value
        .as_object()
        .ok_or(ManifestError::Malformed("a JSON manifest chunk list is not an object"))?;
    if object.len() > MAX_CHUNKS {
        return Err(ManifestError::TooMany {
            what: "chunks",
            limit: MAX_CHUNKS,
        });
    }
    object
        .iter()
        .map(|(key, value)| Ok((guid_hex(key)?, value)))
        .collect::<Result<HashMap<_, _>, _>>()
        .map(Some)
}

fn files(root: &Map<String, Value>) -> Result<Vec<RawFile>, ManifestError> {
    let list = root
        .get("FileManifestList")
        .and_then(Value::as_array)
        .ok_or(ManifestError::Malformed("the JSON manifest has no file list"))?;
    if list.len() > MAX_FILES {
        return Err(ManifestError::TooMany {
            what: "files",
            limit: MAX_FILES,
        });
    }
    let mut parts_total = 0_usize;
    let mut files = Vec::with_capacity(list.len());
    for entry in list {
        let entry = entry
            .as_object()
            .ok_or(ManifestError::Malformed("a JSON manifest file is not an object"))?;
        let name = entry
            .get("Filename")
            .and_then(Value::as_str)
            .ok_or(ManifestError::Malformed("a JSON manifest file has no name"))?;
        let hash = blob(
            entry
                .get("FileHash")
                .ok_or(ManifestError::Malformed("a JSON manifest file has no hash"))?,
        )?;
        let sha1: [u8; 20] = hash
            .try_into()
            .map_err(|_| ManifestError::Malformed("a JSON manifest file hash is not 20 bytes"))?;
        let install_tags = entry.get("InstallTags").and_then(Value::as_array).map_or(0, Vec::len);
        if install_tags > MAX_TAGS_PER_FILE {
            return Err(ManifestError::TooMany {
                what: "install tags of a file",
                limit: MAX_TAGS_PER_FILE,
            });
        }
        let raw_parts = match entry.get("FileChunkParts") {
            None => &[][..],
            Some(value) => value
                .as_array()
                .ok_or(ManifestError::Malformed("a JSON manifest file's parts are not a list"))?,
        };
        parts_total = parts_total.saturating_add(raw_parts.len());
        if parts_total > MAX_PARTS {
            return Err(ManifestError::TooMany {
                what: "chunk parts",
                limit: MAX_PARTS,
            });
        }
        let parts = raw_parts.iter().map(part).collect::<Result<Vec<_>, _>>()?;
        files.push(RawFile {
            name: name.to_owned(),
            symlink_target: String::new(),
            sha1: Sha1Digest(sha1),
            parts,
            install_tags,
        });
    }
    Ok(files)
}

fn part(value: &Value) -> Result<RawPart, ManifestError> {
    let malformed = ManifestError::Malformed("a JSON manifest chunk part is not a GUID, offset and size");
    let part = value.as_object().ok_or(malformed)?;
    let field = |name: &str| {
        part.get(name)
            .ok_or(ManifestError::Malformed("a JSON manifest chunk part lacks a member"))
    };
    let guid = guid_hex(field("Guid")?.as_str().ok_or(ManifestError::Malformed(
        "a JSON manifest chunk part's GUID is not a string",
    ))?)?;
    let narrow = |value: u64| u32::try_from(value).map_err(|_| ManifestError::Malformed("a chunk part is past 4 GiB"));
    Ok(RawPart {
        guid,
        offset: narrow(blob_u64(field("Offset")?)?)?,
        size: narrow(blob_u64(field("Size")?)?)?,
    })
}

/// A blob's bytes, in the order it lists them.
fn blob(value: &Value) -> Result<Vec<u8>, ManifestError> {
    let digits = value
        .as_str()
        .ok_or(ManifestError::Malformed("a JSON manifest number is not a string"))?
        .as_bytes();
    if digits.len() % 3 != 0 || digits.len() > MAX_BLOB_BYTES * 3 || !digits.iter().all(u8::is_ascii_digit) {
        return Err(ManifestError::Malformed(
            "a JSON manifest number is not decimal triplets",
        ));
    }
    digits
        .chunks_exact(3)
        .map(|triplet| {
            let byte = triplet
                .iter()
                .fold(0_u32, |sum, digit| sum * 10 + u32::from(digit - b'0'));
            u8::try_from(byte).map_err(|_| ManifestError::Malformed("a JSON manifest number has a triplet past 255"))
        })
        .collect()
}

/// A blob read as a little-endian number; bytes past the eighth must be zero.
fn blob_u64(value: &Value) -> Result<u64, ManifestError> {
    let bytes = blob(value)?;
    if bytes.iter().skip(8).any(|byte| *byte != 0) {
        return Err(ManifestError::Malformed("a JSON manifest number does not fit 64 bits"));
    }
    Ok(bytes
        .iter()
        .take(8)
        .rev()
        .fold(0_u64, |sum, byte| (sum << 8) | u64::from(*byte)))
}

/// 32 hex digits as four big-endian words, the JSON form's reading (spec, 6.5).
fn guid_hex(text: &str) -> Result<Guid, ManifestError> {
    let malformed = || ManifestError::Malformed("a JSON manifest GUID is not 32 hex digits");
    if text.len() != 32 || !text.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(malformed());
    }
    let mut words = [0_u32; 4];
    for (index, word) in words.iter_mut().enumerate() {
        let digits = text.get(index * 8..index * 8 + 8).ok_or_else(malformed)?;
        *word = u32::from_str_radix(digits, 16).map_err(|_| malformed())?;
    }
    Ok(Guid(words))
}

fn sha_hex(value: &Value) -> Result<Sha1Digest, ManifestError> {
    let malformed = || ManifestError::Malformed("a JSON manifest chunk SHA-1 is not 40 hex digits");
    let text = value.as_str().ok_or_else(malformed)?;
    if text.len() != 40 || !text.is_ascii() {
        return Err(malformed());
    }
    let mut out = [0_u8; 20];
    for (index, byte) in out.iter_mut().enumerate() {
        let digits = text.get(index * 2..index * 2 + 2).ok_or_else(malformed)?;
        *byte = u8::from_str_radix(digits, 16).map_err(|_| malformed())?;
    }
    Ok(Sha1Digest(out))
}
