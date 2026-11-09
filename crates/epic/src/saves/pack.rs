// SPDX-License-Identifier: GPL-3.0-or-later
//! A save folder packed as a snapshot (spec, 9): every file cut into 1 MiB windows, each window a
//! chunk zero-padded to 1 MiB as spec 7.2 says legendary's save chunks are, and a manifest naming
//! them. The result is read back with the same reader a restore uses before anything is uploaded,
//! so that decanter never puts a snapshot it could not restore itself.
//!
//! A chunk's id is the start of its data's SHA-1, so that the same window always has the same
//! chunk path: a chunk the listing already holds is that very chunk, and an upload never needs to
//! replace one.

use std::collections::HashMap;
use std::collections::hash_map::Entry;

use super::SaveError;
use super::folder::{self, LocalFile};
use crate::install::chunk::{self, Want};
use crate::manifest::write::{self, NewChunk, NewFile, NewPart};
use crate::manifest::{Guid, MAX_WINDOW, Sha1Digest, parse_manifest};

/// Spec 6.4: the group a chunk files under is the CRC-32 of its id's 16 bytes, modulo 100.
const GROUPS: u32 = 100;

pub(super) struct PackedChunk {
    /// The chunk's path under the title's folder (spec, 6.4).
    pub(super) path: String,
    pub(super) file: Vec<u8>,
}

pub(super) struct Packed {
    pub(super) manifest: Vec<u8>,
    pub(super) chunks: Vec<PackedChunk>,
    pub(super) files: usize,
    pub(super) bytes: u64,
}

pub(super) fn pack(app_name: &str, local: &[LocalFile]) -> Result<Packed, SaveError> {
    let window = usize::try_from(MAX_WINDOW).unwrap_or(usize::MAX);
    let packing = |err: std::io::Error| SaveError::Local(format!("packing the save: {err}"));
    let mut new_chunks = Vec::new();
    let mut encoded: HashMap<Guid, Vec<u8>> = HashMap::new();
    let mut new_files = Vec::with_capacity(local.len());
    let mut bytes = 0_u64;
    for file in local {
        let body = folder::read(&file.path)?;
        bytes = bytes.saturating_add(u64::try_from(body.len()).unwrap_or(u64::MAX));
        let mut parts = Vec::with_capacity(body.len().div_ceil(window));
        for piece in body.chunks(window) {
            let mut data = piece.to_vec();
            data.resize(window, 0);
            let guid = content_guid(&data);
            if let Entry::Vacant(slot) = encoded.entry(guid) {
                let made = chunk::encode(guid, &data).map_err(packing)?;
                new_chunks.push(NewChunk {
                    guid,
                    rolling_hash: made.rolling_hash,
                    sha1: made.sha1,
                    group: group_of(guid),
                    window_size: MAX_WINDOW,
                    file_size: u64::try_from(made.file.len()).unwrap_or(u64::MAX),
                });
                slot.insert(made.file);
            }
            parts.push(NewPart {
                guid,
                offset: 0,
                size: u32::try_from(piece.len()).unwrap_or(MAX_WINDOW),
            });
        }
        new_files.push(NewFile {
            name: file.relative.as_str().to_owned(),
            sha1: Sha1Digest::of(&body),
            parts,
        });
    }
    let manifest = write::write(app_name, &new_chunks, &new_files).map_err(packing)?;
    let own = |what: String| SaveError::Corrupt(format!("decanter's own snapshot does not read back: {what}"));
    let parsed = parse_manifest(&manifest).map_err(|err| own(err.to_string()))?;
    let mut chunks = Vec::with_capacity(parsed.chunks().len());
    for listed in parsed.chunks() {
        let file = encoded
            .remove(&listed.guid)
            .ok_or_else(|| own("a chunk it names was not packed".to_owned()))?;
        chunk::decode(&file, &Want::of(listed)).map_err(|fault| own(format!("a chunk is {fault}")))?;
        chunks.push(PackedChunk {
            path: chunk::path(parsed.feature_level(), listed),
            file,
        });
    }
    Ok(Packed {
        manifest,
        chunks,
        files: new_files.len(),
        bytes,
    })
}

fn content_guid(data: &[u8]) -> Guid {
    let digest = Sha1Digest::of(data);
    let mut words = [0_u32; 4];
    for (word, bytes) in words.iter_mut().zip(digest.as_bytes().chunks_exact(4)) {
        let mut array = [0_u8; 4];
        array.copy_from_slice(bytes);
        *word = u32::from_le_bytes(array);
    }
    Guid(words)
}

fn group_of(guid: Guid) -> u8 {
    let mut crc = flate2::Crc::new();
    crc.update(&guid.to_le_bytes());
    u8::try_from(crc.sum() % GROUPS).unwrap_or_default()
}
