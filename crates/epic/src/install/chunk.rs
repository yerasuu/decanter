// SPDX-License-Identifier: GPL-3.0-or-later
//! One chunk file from the CDN: its header (spec, 7.1), its payload inflated (7.2), and its data
//! checked against both the chunk's own header and the manifest before any byte of it is written.
//!
//! legendary checks none of this (spec, 7.2 step 5). Here the SHA-1, from the chunk header or the
//! manifest, decides: a chunk whose SHA-1 is right is good whatever spec 7.2's rolling hash says,
//! since how that hash covers a short last window is still open (spec, Q15), and a disagreement is
//! counted for the owner instead. Only a chunk with no SHA-1 to check is held to the rolling hash.

use std::io::Read as _;

use flate2::read::ZlibDecoder;

use crate::manifest::{Chunk, Guid, MAX_WINDOW, Sha1Digest};

const MAGIC: u32 = 0xB1FE_3AA2;
const COMPRESSED: u8 = 0x1;
const ENCRYPTED: u8 = 0x2;
const HASH_SHA1: u8 = 0x2;
/// The polynomial of spec 7.2's rolling hash, a variation on CRC-64-ECMA.
const POLYNOMIAL: u64 = 0xC96C_5795_D787_0F42;
const ROLLING_TABLE: [u64; 256] = rolling_table();

/// What a chunk needs to be fetched and checked, copied out of the manifest so a download task owns
/// it.
#[derive(Debug, Clone, Copy)]
pub(super) struct Want {
    pub(super) guid: Guid,
    pub(super) rolling_hash: u64,
    pub(super) sha1: Option<Sha1Digest>,
    pub(super) window_size: u32,
}

impl Want {
    pub(super) const fn of(chunk: &Chunk) -> Self {
        Self {
            guid: chunk.guid,
            rolling_hash: chunk.rolling_hash,
            sha1: chunk.sha1,
            window_size: chunk.window_size,
        }
    }
}

/// Why a chunk file was refused.
#[derive(Debug)]
pub(super) enum Fault {
    /// Not a chunk file of a shape spec 7.1 describes.
    Malformed(&'static str),
    /// Its data does not match what the manifest or its header says; names the check.
    Mismatch(&'static str),
    /// Encrypted, which decanter does not decrypt (spec, 5.4).
    Encrypted,
}

impl core::fmt::Display for Fault {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Malformed(what) => write!(f, "malformed: {what}"),
            Self::Mismatch(what) => write!(f, "does not match: {what}"),
            Self::Encrypted => f.write_str("encrypted"),
        }
    }
}

/// A chunk's data, checked.
pub(super) struct Verified {
    pub(super) data: Vec<u8>,
    /// The SHA-1 passed but spec 7.2's rolling hash came out otherwise, over the data and over it
    /// padded to 1 MiB.
    pub(super) is_rolling_off: bool,
}

struct Header {
    header_size: usize,
    compressed_size: usize,
    guid: Guid,
    rolling_hash: u64,
    stored_as: u8,
    sha1: Option<Sha1Digest>,
    hash_type: u8,
    uncompressed_size: usize,
}

/// Reads, inflates and checks one chunk file as downloaded.
pub(super) fn decode(raw: &[u8], want: &Want) -> Result<Verified, Fault> {
    let header = header(raw)?;
    if header.guid != want.guid {
        return Err(Fault::Mismatch("the chunk header names another chunk"));
    }
    if header.stored_as & !(COMPRESSED | ENCRYPTED) != 0 {
        return Err(Fault::Malformed("unknown storage flags"));
    }
    if header.stored_as & ENCRYPTED != 0 {
        return Err(Fault::Encrypted);
    }
    let payload = raw
        .get(header.header_size..)
        .ok_or(Fault::Malformed("the header is longer than the file"))?;
    if payload.len() != header.compressed_size {
        return Err(Fault::Malformed("the payload is not the size its header declares"));
    }
    if header.uncompressed_size != usize::try_from(want.window_size).unwrap_or(usize::MAX) {
        return Err(Fault::Mismatch("the chunk's size differs from the manifest's"));
    }
    let data = if header.stored_as & COMPRESSED != 0 {
        inflate(payload, header.uncompressed_size)?
    } else if payload.len() == header.uncompressed_size {
        payload.to_vec()
    } else {
        return Err(Fault::Malformed("an uncompressed payload is not the chunk's size"));
    };
    let sha1 = Sha1Digest::of(&data);
    let header_sha1 = header.sha1.filter(|_| header.hash_type & HASH_SHA1 != 0);
    let shas = [header_sha1, want.sha1];
    if shas.iter().flatten().any(|expected| *expected != sha1) {
        return Err(Fault::Mismatch("SHA-1"));
    }
    let has_sha1 = shas.iter().any(Option::is_some);
    let is_rolling_right = header.rolling_hash == want.rolling_hash && rolling_matches(&data, want.rolling_hash);
    if !has_sha1 && !is_rolling_right {
        return Err(Fault::Mismatch("rolling hash"));
    }
    Ok(Verified {
        data,
        is_rolling_off: !is_rolling_right,
    })
}

fn header(raw: &[u8]) -> Result<Header, Fault> {
    let mut pos = 0_usize;
    let mut take = |len: usize| -> Result<&[u8], Fault> {
        let end = pos.saturating_add(len);
        let bytes = raw
            .get(pos..end)
            .ok_or(Fault::Malformed("the header runs past the file"))?;
        pos = end;
        Ok(bytes)
    };
    if le_u32(take(4)?) != MAGIC {
        return Err(Fault::Malformed("not a chunk file"));
    }
    let version = le_u32(take(4)?);
    let header_size = to_usize(le_u32(take(4)?));
    let compressed_size = to_usize(le_u32(take(4)?));
    let guid = Guid([le_u32(take(4)?), le_u32(take(4)?), le_u32(take(4)?), le_u32(take(4)?)]);
    let rolling_hash = u64::from_le_bytes(array(take(8)?));
    let stored_as = take(1)?.first().copied().unwrap_or_default();
    let (sha1, hash_type) = if version >= 2 {
        let sha1 = Sha1Digest(array(take(20)?));
        (Some(sha1), take(1)?.first().copied().unwrap_or_default())
    } else {
        (None, 0)
    };
    let uncompressed_size = if version >= 3 {
        to_usize(le_u32(take(4)?))
    } else {
        to_usize(MAX_WINDOW)
    };
    if version >= 4 {
        take(32)?;
    }
    // Spec 7.1: a header whose declared size is not what its version's fields add up to is
    // malformed, which also refuses a later version with fields decanter does not know.
    if pos != header_size {
        return Err(Fault::Malformed("the header's size does not match its version"));
    }
    Ok(Header {
        header_size,
        compressed_size,
        guid,
        rolling_hash,
        stored_as,
        sha1,
        hash_type,
        uncompressed_size,
    })
}

fn inflate(payload: &[u8], size: usize) -> Result<Vec<u8>, Fault> {
    let mut data = Vec::with_capacity(size);
    let limit = u64::try_from(size).unwrap_or(u64::MAX).saturating_add(1);
    ZlibDecoder::new(payload)
        .take(limit)
        .read_to_end(&mut data)
        .map_err(|_| Fault::Malformed("the payload does not inflate"))?;
    if data.len() == size {
        Ok(data)
    } else {
        Err(Fault::Malformed(
            "the payload inflates to another size than its header declares",
        ))
    }
}

/// Whether spec 7.2's rolling hash of `data`, or of `data` padded with zeros to 1 MiB as legendary's
/// writer pads (spec, 7.2 step 4), is `expected`.
fn rolling_matches(data: &[u8], expected: u64) -> bool {
    let hash = rolling(data);
    if hash == expected {
        return true;
    }
    let padding = to_usize(MAX_WINDOW).saturating_sub(data.len());
    padding > 0 && (0..padding).fold(hash, |sum, _| step(sum, 0)) == expected
}

fn rolling(data: &[u8]) -> u64 {
    data.iter().fold(0, |sum, byte| step(sum, *byte))
}

fn step(sum: u64, byte: u8) -> u64 {
    sum.rotate_left(1) ^ ROLLING_TABLE.get(usize::from(byte)).copied().unwrap_or_default()
}

const fn rolling_table() -> [u64; 256] {
    let mut table = [0_u64; 256];
    let mut index = 0;
    while index < 256 {
        let mut value = index as u64;
        let mut bit = 0;
        while bit < 8 {
            value = if value & 1 == 1 {
                (value >> 1) ^ POLYNOMIAL
            } else {
                value >> 1
            };
            bit += 1;
        }
        table[index] = value;
        index += 1;
    }
    table
}

/// The chunk's path under a CDN base by the manifest's feature level (spec, 6.4).
pub(super) fn path(feature_level: u32, chunk: &Chunk) -> String {
    let dir = match feature_level {
        22.. => "ChunksV5",
        15..=21 => "ChunksV4",
        6..=14 => "ChunksV3",
        3..=5 => "ChunksV2",
        _ => "Chunks",
    };
    if feature_level >= 22 {
        // Encrypted chunks are refused with their manifest, so the secret segment is always
        // `plain`.
        format!(
            "{dir}/plain/{:02}/{}_{}.chunk",
            chunk.group,
            base64url(&chunk.rolling_hash.to_le_bytes()),
            base64url(&chunk.guid.to_le_bytes())
        )
    } else {
        format!(
            "{dir}/{:02}/{:016X}_{}.chunk",
            chunk.group, chunk.rolling_hash, chunk.guid
        )
    }
}

/// URL-safe base64 without padding, as spec 6.4 names chunks of feature level 22 and later.
fn base64url(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let symbol = |bits: u32| char::from(ALPHABET.get(to_usize(bits & 0x3f)).copied().unwrap_or(b'A'));
    let mut text = String::with_capacity(bytes.len().div_ceil(3).saturating_mul(4));
    for group in bytes.chunks(3) {
        let (first, second, third) = match *group {
            [first, second, third] => (first, Some(second), Some(third)),
            [first, second] => (first, Some(second), None),
            [first] => (first, None, None),
            _ => continue,
        };
        let bits = (u32::from(first) << 16_u32)
            | (u32::from(second.unwrap_or(0_u8)) << 8_u32)
            | u32::from(third.unwrap_or(0_u8));
        text.push(symbol(bits >> 18_u32));
        text.push(symbol(bits >> 12_u32));
        if second.is_some() {
            text.push(symbol(bits >> 6_u32));
        }
        if third.is_some() {
            text.push(symbol(bits));
        }
    }
    text
}

fn array<const N: usize>(bytes: &[u8]) -> [u8; N] {
    let mut out = [0_u8; N];
    out.copy_from_slice(bytes.get(..N).unwrap_or(&[0; N][..]));
    out
}

fn le_u32(bytes: &[u8]) -> u32 {
    u32::from_le_bytes(array(bytes))
}

fn to_usize(value: u32) -> usize {
    usize::try_from(value).unwrap_or(usize::MAX)
}
