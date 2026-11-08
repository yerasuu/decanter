// SPDX-License-Identifier: GPL-3.0-or-later
//! The binary manifest: a header (spec, 6.1), then a body, zlib-compressed when the header says,
//! holding the meta, chunk list, file list and custom field sections in that order (spec, 6.3).
//! The chunk and file lists are columnar: every entry's first field, then every entry's second.

use std::io::Read as _;

use flate2::read::ZlibDecoder;

use super::reader::Reader;
use super::{
    Chunk, Guid, MAX_BODY_LEN, MAX_CHUNKS, MAX_CUSTOM_FIELDS, MAX_FILES, MAX_PARTS, MAX_PREREQ_IDS, MAX_TAGS_PER_FILE,
    MAX_VERSION, ManifestError, Parsed, RawFile, RawPart, Sha1Digest,
};

pub(super) const MAGIC: u32 = 0x44BE_C00C;

const COMPRESSED: u8 = 0x1;
const ENCRYPTED: u8 = 0x2;

/// From this version the header carries the secret id and the AES-GCM tag, and from this feature
/// level the chunk list three more columns (spec, 6.1 and 6.3).
const SECRET_VERSION: u32 = 22;

/// The record size of a chunk part as spec 6.3 lays it out; a larger one is skipped past.
const PART_RECORD: usize = 28;

pub(super) fn parse(raw: &[u8]) -> Result<Parsed, ManifestError> {
    let mut header = Reader::new(raw);
    if header.u32()? != MAGIC {
        return Err(ManifestError::UnknownFormat);
    }
    let header_size = usize::try_from(header.u32()?).unwrap_or(usize::MAX);
    let size_uncompressed = usize::try_from(header.u32()?).unwrap_or(usize::MAX);
    let size_compressed = usize::try_from(header.u32()?).unwrap_or(usize::MAX);
    let body_sha1 = Sha1Digest(header.array()?);
    let stored_as = header.u8()?;
    let version = header.u32()?;
    if version > MAX_VERSION {
        return Err(ManifestError::UnsupportedVersion(version));
    }
    if version >= SECRET_VERSION {
        header.take(32)?;
    }
    if stored_as & !(COMPRESSED | ENCRYPTED) != 0 {
        return Err(ManifestError::UnknownStorage(stored_as));
    }
    if stored_as & ENCRYPTED != 0 {
        return Err(ManifestError::Encrypted);
    }
    if header_size < header.pos() {
        return Err(ManifestError::Malformed("the header is shorter than its own fields"));
    }
    if header_size > header.pos() {
        tracing::info!(
            version,
            header_size,
            read = header.pos(),
            "epic manifest header longer than its known fields, skipping the rest"
        );
    }
    let body = raw
        .get(header_size..)
        .ok_or(ManifestError::Malformed("the header is longer than the file"))?;
    let inflated;
    let body = if stored_as & COMPRESSED != 0 {
        if body.len() != size_compressed {
            tracing::info!(
                declared = size_compressed,
                actual = body.len(),
                "epic manifest body is not the compressed size its header declares"
            );
        }
        inflated = inflate(body, size_uncompressed)?;
        inflated.as_slice()
    } else {
        if body.len() > MAX_BODY_LEN {
            return Err(ManifestError::TooLarge {
                what: "body",
                limit: MAX_BODY_LEN,
            });
        }
        body
    };
    // Spec 6.1 checks this only for a compressed body; it is the hash of the body as stored once
    // inflated, so an uncompressed body is held to it too, and a mismatch is reported, not skipped.
    if Sha1Digest::of(body) != body_sha1 {
        return Err(ManifestError::BodyHash);
    }
    sections(body, version)
}

/// Inflates a zlib body of `declared` bytes, never past it nor past [`MAX_BODY_LEN`].
fn inflate(body: &[u8], declared: usize) -> Result<Vec<u8>, ManifestError> {
    if declared > MAX_BODY_LEN {
        return Err(ManifestError::TooLarge {
            what: "body",
            limit: MAX_BODY_LEN,
        });
    }
    let mut out = Vec::with_capacity(declared);
    let limit = u64::try_from(declared).unwrap_or(u64::MAX).saturating_add(1);
    ZlibDecoder::new(body)
        .take(limit)
        .read_to_end(&mut out)
        .map_err(ManifestError::Inflate)?;
    if out.len() != declared {
        return Err(ManifestError::Malformed(
            "the body inflates to another size than its header declares",
        ));
    }
    Ok(out)
}

fn sections(body: &[u8], version: u32) -> Result<Parsed, ManifestError> {
    let mut body = Reader::new(body);
    let mut meta = body.section("the meta section runs past the end of the body")?;
    let data_version = meta.u8()?;
    let feature_level = meta.u32()?;
    if feature_level > MAX_VERSION {
        return Err(ManifestError::UnsupportedVersion(feature_level));
    }
    if feature_level != version {
        tracing::debug!(
            version,
            feature_level,
            "epic manifest feature level differs from its header version"
        );
    }
    if meta.u8()? != 0 {
        return Err(ManifestError::FileData);
    }
    let _app_id = meta.u32()?;
    let app_name = meta.fstring()?;
    let build_version = meta.fstring()?;
    let launch_exe = meta.fstring()?;
    let launch_command = meta.fstring()?;
    // The prerequisite installers and, from data version 1 and 2, the build id and the uninstall
    // action are read past: nothing installs prerequisites yet, and the section's size skips them.
    let prereqs = meta.count(4, MAX_PREREQ_IDS, "prerequisite ids")?;
    for _ in 0..prereqs {
        meta.fstring()?;
    }
    tracing::debug!(data_version, prereqs, "read the epic manifest's meta section");

    let chunks = chunk_list(&mut body, feature_level)?;
    let files = file_list(&mut body)?;
    if body.remaining() > 0 {
        let mut custom = body.section("the custom fields section runs past the end of the body")?;
        let _version = custom.u8()?;
        let count = custom.count(8, MAX_CUSTOM_FIELDS, "custom fields")?;
        for _ in 0..count.saturating_mul(2) {
            custom.fstring()?;
        }
        tracing::debug!(count, "read the epic manifest's custom fields");
    }
    if body.remaining() > 0 {
        tracing::info!(
            unread = body.remaining(),
            "epic manifest body has bytes past its known sections"
        );
    }
    Ok(Parsed {
        feature_level,
        app_name,
        build_version,
        launch_exe,
        launch_command,
        chunks,
        files,
    })
}

fn chunk_list(body: &mut Reader<'_>, feature_level: u32) -> Result<Vec<Chunk>, ManifestError> {
    let mut list = body.section("the chunk list runs past the end of the body")?;
    let _version = list.u8()?;
    let has_secrets = feature_level >= SECRET_VERSION;
    let entry = if has_secrets { 57 + 36 } else { 57 };
    let count = list.count(entry, MAX_CHUNKS, "chunks")?;
    let guids = column(count, || list.guid())?;
    let hashes = column(count, || list.u64())?;
    let shas = column(count, || list.array::<20>())?;
    let groups = column(count, || list.u8())?;
    let windows = column(count, || list.u32())?;
    let sizes = column(count, || list.i64())?;
    let secrets = if has_secrets {
        let secrets = column(count, || list.guid())?;
        // The compressed window sizes and the encryption tags: only an encrypted chunk needs them,
        // and those are refused.
        list.take(count.saturating_mul(20))?;
        secrets
    } else {
        vec![Guid([0; 4]); count]
    };
    // The download sizes are only checked: a chunk file is bounded by its own cap, which a manifest
    // value could only lower.
    if sizes.iter().any(|size| *size < 0) {
        return Err(ManifestError::Malformed("a chunk has a negative file size"));
    }
    let mut chunks = Vec::with_capacity(count);
    for (index, guid) in guids.into_iter().enumerate() {
        chunks.push(Chunk {
            guid,
            rolling_hash: hashes.get(index).copied().unwrap_or_default(),
            sha1: shas.get(index).map(|sha| Sha1Digest(*sha)),
            group: groups.get(index).copied().unwrap_or_default(),
            window_size: windows.get(index).copied().unwrap_or_default(),
            secret: secrets.get(index).copied().unwrap_or(Guid([0; 4])),
        });
    }
    Ok(chunks)
}

fn file_list(body: &mut Reader<'_>) -> Result<Vec<RawFile>, ManifestError> {
    let mut list = body.section("the file list runs past the end of the body")?;
    let _version = list.u8()?;
    let count = list.count(37, MAX_FILES, "files")?;
    let names = column(count, || list.fstring())?;
    let symlinks = column(count, || list.fstring())?;
    let hashes = column(count, || list.array::<20>())?;
    let _flags = column(count, || list.u8())?;
    let mut tags = Vec::with_capacity(count);
    for _ in 0..count {
        let n = list.count(4, MAX_TAGS_PER_FILE, "install tags of a file")?;
        for _ in 0..n {
            list.fstring()?;
        }
        tags.push(n);
    }
    let mut parts_total = 0_usize;
    let mut all_parts = Vec::with_capacity(count);
    for _ in 0..count {
        let n = list.count(PART_RECORD, MAX_PARTS, "chunk parts of a file")?;
        parts_total = parts_total.saturating_add(n);
        if parts_total > MAX_PARTS {
            return Err(ManifestError::TooMany {
                what: "chunk parts",
                limit: MAX_PARTS,
            });
        }
        let mut parts = Vec::with_capacity(n);
        for _ in 0..n {
            let record = usize::try_from(list.u32()?).unwrap_or(usize::MAX);
            if record < PART_RECORD {
                return Err(ManifestError::Malformed(
                    "a chunk part record is shorter than its fields",
                ));
            }
            parts.push(RawPart {
                guid: list.guid()?,
                offset: list.u32()?,
                size: list.u32()?,
            });
            list.take(record.saturating_sub(PART_RECORD))?;
        }
        all_parts.push(parts);
    }
    // MD5s, MIME types and SHA-256s of section versions 1 and 2 follow; the section's size skips them,
    // since the SHA-1 is what every file is verified by.
    Ok(names
        .into_iter()
        .zip(symlinks)
        .zip(hashes)
        .zip(tags)
        .zip(all_parts)
        .map(|((((name, symlink_target), sha1), install_tags), parts)| RawFile {
            name,
            symlink_target,
            sha1: Sha1Digest(sha1),
            parts,
            install_tags,
        })
        .collect())
}

fn column<T>(count: usize, mut read: impl FnMut() -> Result<T, ManifestError>) -> Result<Vec<T>, ManifestError> {
    let mut values = Vec::with_capacity(count);
    for _ in 0..count {
        values.push(read()?);
    }
    Ok(values)
}
