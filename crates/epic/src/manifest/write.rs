// SPDX-License-Identifier: GPL-3.0-or-later
//! A binary manifest written for a save snapshot: the header of spec 6.1 and the four sections of
//! 6.3 in the oldest layout each has, which [`super::parse_manifest`] reads back before anything
//! of it is uploaded.
//!
//! Spec 9 says the snapshots legendary writes are version 17 or 18; 18 keeps the chunk folder at
//! `ChunksV4` and the header and chunk list free of the secret fields of 22.

use std::io::{self, Write as _};

use flate2::Compression;
use flate2::write::ZlibEncoder;

use super::{Guid, Sha1Digest, binary::MAGIC};

pub(crate) const SNAPSHOT_FEATURE_LEVEL: u32 = 18;

const HEADER_SIZE: u32 = 41;
const COMPRESSED: u8 = 0x1;
const PART_RECORD: u32 = 28;

pub(crate) struct NewChunk {
    pub(crate) guid: Guid,
    pub(crate) rolling_hash: u64,
    pub(crate) sha1: Sha1Digest,
    pub(crate) group: u8,
    pub(crate) window_size: u32,
    /// The size of the chunk file as uploaded.
    pub(crate) file_size: u64,
}

pub(crate) struct NewFile {
    /// A path [`decanter_runtime::install::ManifestPath`] accepted, `/`-separated.
    pub(crate) name: String,
    pub(crate) sha1: Sha1Digest,
    pub(crate) parts: Vec<NewPart>,
}

pub(crate) struct NewPart {
    pub(crate) guid: Guid,
    pub(crate) offset: u32,
    pub(crate) size: u32,
}

pub(crate) fn write(app_name: &str, chunks: &[NewChunk], files: &[NewFile]) -> io::Result<Vec<u8>> {
    let mut body = Vec::new();
    section(&mut body, &meta(app_name))?;
    section(&mut body, &chunk_list(chunks)?)?;
    section(&mut body, &file_list(files)?)?;
    // The custom fields: version 0, none.
    section(&mut body, &[0, 0, 0, 0, 0])?;

    let mut encoder = ZlibEncoder::new(Vec::with_capacity(body.len()), Compression::default());
    encoder.write_all(&body)?;
    let compressed = encoder.finish()?;
    let mut out = Vec::with_capacity(compressed.len().saturating_add(64));
    out.extend_from_slice(&MAGIC.to_le_bytes());
    out.extend_from_slice(&HEADER_SIZE.to_le_bytes());
    out.extend_from_slice(&len_u32(body.len())?.to_le_bytes());
    out.extend_from_slice(&len_u32(compressed.len())?.to_le_bytes());
    out.extend_from_slice(Sha1Digest::of(&body).as_bytes());
    out.push(COMPRESSED);
    out.extend_from_slice(&SNAPSHOT_FEATURE_LEVEL.to_le_bytes());
    out.extend_from_slice(&compressed);
    Ok(out)
}

/// Data version 0: no build id, which spec 6.3 derives from the other fields when absent.
fn meta(app_name: &str) -> Vec<u8> {
    let mut meta = vec![0];
    meta.extend_from_slice(&SNAPSHOT_FEATURE_LEVEL.to_le_bytes());
    meta.push(0);
    meta.extend_from_slice(&0_u32.to_le_bytes());
    fstring(&mut meta, app_name);
    // The build version, launch program and launch command, which a save has none of.
    fstring(&mut meta, "");
    fstring(&mut meta, "");
    fstring(&mut meta, "");
    // No prerequisite ids, then their empty name, path and arguments.
    meta.extend_from_slice(&0_u32.to_le_bytes());
    fstring(&mut meta, "");
    fstring(&mut meta, "");
    fstring(&mut meta, "");
    meta
}

fn chunk_list(chunks: &[NewChunk]) -> io::Result<Vec<u8>> {
    let mut list = vec![0];
    list.extend_from_slice(&len_u32(chunks.len())?.to_le_bytes());
    for chunk in chunks {
        list.extend_from_slice(&chunk.guid.to_le_bytes());
    }
    for chunk in chunks {
        list.extend_from_slice(&chunk.rolling_hash.to_le_bytes());
    }
    for chunk in chunks {
        list.extend_from_slice(chunk.sha1.as_bytes());
    }
    for chunk in chunks {
        list.push(chunk.group);
    }
    for chunk in chunks {
        list.extend_from_slice(&chunk.window_size.to_le_bytes());
    }
    for chunk in chunks {
        let size = i64::try_from(chunk.file_size).map_err(|_| io::Error::other("a chunk file size overflows"))?;
        list.extend_from_slice(&size.to_le_bytes());
    }
    Ok(list)
}

fn file_list(files: &[NewFile]) -> io::Result<Vec<u8>> {
    let mut list = vec![0];
    list.extend_from_slice(&len_u32(files.len())?.to_le_bytes());
    for file in files {
        fstring(&mut list, &file.name);
    }
    for _ in files {
        fstring(&mut list, "");
    }
    for file in files {
        list.extend_from_slice(file.sha1.as_bytes());
    }
    for _ in files {
        list.push(0);
    }
    for _ in files {
        list.extend_from_slice(&0_u32.to_le_bytes());
    }
    for file in files {
        list.extend_from_slice(&len_u32(file.parts.len())?.to_le_bytes());
        for part in &file.parts {
            list.extend_from_slice(&PART_RECORD.to_le_bytes());
            list.extend_from_slice(&part.guid.to_le_bytes());
            list.extend_from_slice(&part.offset.to_le_bytes());
            list.extend_from_slice(&part.size.to_le_bytes());
        }
    }
    Ok(list)
}

/// A section as spec 6.3 frames it: its size, counting the size field itself, then its bytes.
fn section(body: &mut Vec<u8>, content: &[u8]) -> io::Result<()> {
    let size = len_u32(content.len().saturating_add(4))?;
    body.extend_from_slice(&size.to_le_bytes());
    body.extend_from_slice(content);
    Ok(())
}

/// Spec 6.2's `FString`: ASCII as NUL-ended bytes under a positive length, anything else as NUL-ended
/// UTF-16LE under a negative count of units.
fn fstring(out: &mut Vec<u8>, text: &str) {
    if text.is_empty() {
        out.extend_from_slice(&0_i32.to_le_bytes());
    } else if text.is_ascii() {
        let len = i32::try_from(text.len().saturating_add(1)).unwrap_or(i32::MAX);
        out.extend_from_slice(&len.to_le_bytes());
        out.extend_from_slice(text.as_bytes());
        out.push(0);
    } else {
        let units: Vec<u16> = text.encode_utf16().chain([0]).collect();
        let len = i32::try_from(units.len()).map_or(i32::MIN, |count| 0_i32.saturating_sub(count));
        out.extend_from_slice(&len.to_le_bytes());
        for unit in units {
            out.extend_from_slice(&unit.to_le_bytes());
        }
    }
}

fn len_u32(len: usize) -> io::Result<u32> {
    u32::try_from(len).map_err(|_| io::Error::other("a save snapshot is too large for a manifest"))
}
