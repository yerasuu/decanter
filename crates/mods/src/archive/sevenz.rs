//! 7z through `sevenz-rust2`, whose path handling is not documented: every name goes through
//! [`super::entry_path`], and entry types are read from the attributes 7-Zip and p7zip write.

use std::fs;
use std::io;
use std::path::Path;

use sevenz_rust2::{ArchiveReader, Block, EncoderMethod, Password};

use super::{RawEntry, Sink, check_ratio};
use crate::{Error, MAX_ENTRIES, Result};

/// [File attribute constants](https://learn.microsoft.com/en-us/windows/win32/fileio/file-attribute-constants).
const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
/// p7zip's flag that the high 16 bits hold a Unix mode.
const UNIX_EXTENSION: u32 = 0x8000;
const S_IFMT: u32 = 0o170_000;
const S_IFREG: u32 = 0o100_000;
const S_IFDIR: u32 = 0o040_000;
/// 7-Zip's own largest dictionary on 64-bit systems; a decoder asking for more than this was not
/// written by it and could exhaust memory before a byte is out.
const MAX_DECODER_MEMORY: u64 = 1536 << 20;
/// The methods and filters the features in the workspace's `Cargo.toml` build a decoder for,
/// besides LZMA, LZMA2 and `PPMd`, which are checked for their memory.
const DECODED: [&[u8]; 12] = [
    EncoderMethod::ID_COPY,
    EncoderMethod::ID_DELTA,
    EncoderMethod::ID_DEFLATE,
    EncoderMethod::ID_BCJ_X86,
    EncoderMethod::ID_BCJ2,
    EncoderMethod::ID_BCJ_PPC,
    EncoderMethod::ID_BCJ_IA64,
    EncoderMethod::ID_BCJ_ARM,
    EncoderMethod::ID_BCJ_ARM64,
    EncoderMethod::ID_BCJ_ARM_THUMB,
    EncoderMethod::ID_BCJ_SPARC,
    EncoderMethod::ID_BCJ_RISCV,
];

fn open(source: &Path) -> Result<ArchiveReader<fs::File>> {
    let file = fs::File::open(source).map_err(Error::io(source))?;
    ArchiveReader::new(file, Password::empty()).map_err(|err| archive_error(source, err))
}

fn archive_error(source: &Path, err: sevenz_rust2::Error) -> Error {
    match err {
        sevenz_rust2::Error::Io(err, _) | sevenz_rust2::Error::FileOpen(err, _) => Error::io(source)(err),
        sevenz_rust2::Error::PasswordRequired | sevenz_rust2::Error::MaybeBadPassword(_) => Error::Encrypted,
        other => Error::Archive(other.to_string()),
    }
}

pub(super) fn list(source: &Path) -> Result<Vec<RawEntry>> {
    let reader = open(source)?;
    let archive = reader.archive();
    if archive.files.len() > MAX_ENTRIES {
        return Err(Error::TooManyEntries);
    }
    for block in &archive.blocks {
        check_coders(block)?;
    }
    check_block_ratios(archive)?;
    let mut entries = Vec::with_capacity(archive.files.len());
    for file in &archive.files {
        let attributes = file.has_windows_attributes.then_some(file.windows_attributes);
        let is_special = file.is_anti_item
            || attributes.is_some_and(|attributes| {
                attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0
                    || (attributes & UNIX_EXTENSION != 0
                        && !matches!((attributes >> 16_u32) & S_IFMT, 0 | S_IFREG | S_IFDIR))
            });
        if is_special {
            return Err(Error::LinkEntry(file.name.clone()));
        }
        entries.push(RawEntry {
            name: file.name.clone(),
            is_dir: file.is_directory,
            size: file.size,
            packed: None,
        });
    }
    Ok(entries)
}

/// Encryption is refused before decoding, and so is a dictionary larger than 7-Zip writes.
fn check_coders(block: &Block) -> Result<()> {
    for coder in &block.coders {
        let id = coder.encoder_method_id();
        let properties = coder.properties();
        let memory = if id == EncoderMethod::ID_AES256_SHA256 {
            return Err(Error::Encrypted);
        } else if id == EncoderMethod::ID_LZMA || id == EncoderMethod::ID_PPMD {
            // LZMA: lc/lp/pb, then the dictionary; PPMd: the order, then its memory.
            properties
                .get(1..5)
                .and_then(|bytes| <[u8; 4]>::try_from(bytes).ok())
                .map(|bytes| u64::from(u32::from_le_bytes(bytes)))
        } else if id == EncoderMethod::ID_LZMA2 {
            properties.first().map(|&bits| lzma2_dictionary(bits))
        } else if DECODED.contains(&id) {
            None
        } else {
            // Refused here rather than halfway through the extraction.
            let method =
                EncoderMethod::by_id(id).map_or_else(|| format!("{id:02x?}"), |method| method.name().to_owned());
            return Err(Error::Archive(format!(
                "its compression method {method} isn't supported"
            )));
        };
        if let Some(memory) = memory.filter(|&memory| memory > MAX_DECODER_MEMORY) {
            return Err(Error::Memory(memory >> 20));
        }
    }
    Ok(())
}

/// The dictionary size an LZMA2 property byte encodes, per the xz format's LZMA2 filter flags.
fn lzma2_dictionary(bits: u8) -> u64 {
    if bits >= 40 {
        return u64::from(u32::MAX);
    }
    (2_u64 | u64::from(bits & 1)) << (u32::from(bits >> 1_u8).saturating_add(11))
}

/// A solid block decodes as one stream, so its files are checked together: what they expand to
/// against what the block takes in the archive, which only its first file records.
fn check_block_ratios(archive: &sevenz_rust2::Archive) -> Result<()> {
    let mut by_block: Vec<(u64, u64, Option<&str>)> = vec![(0, 0, None); archive.blocks.len()];
    for (file, block) in archive.files.iter().zip(&archive.stream_map.file_block_index) {
        let Some(totals) = block.and_then(|index| by_block.get_mut(index)) else {
            continue;
        };
        totals.0 = totals.0.saturating_add(file.size);
        totals.1 = totals.1.saturating_add(file.compressed_size);
        totals.2.get_or_insert(file.name.as_str());
    }
    for (size, packed, name) in by_block {
        check_ratio(name.unwrap_or_default(), size, packed)?;
    }
    Ok(())
}

pub(super) fn extract(source: &Path, sink: &mut Sink<'_>) -> Result<()> {
    let mut reader = open(source)?;
    let mut failed = None;
    let decoded = reader.for_each_entries(|entry, data| {
        if failed.is_some() {
            return Err(sevenz_rust2::Error::Other("stopped by decanter".into()));
        }
        if !sink.wants(&entry.name) {
            // Read through all the same, since a solid block's later files decode after it.
            io::copy(data, &mut io::sink())?;
            return Ok(true);
        }
        match sink.write(&entry.name, data) {
            Ok(()) => Ok(true),
            // An error rather than `Ok(false)`, which only ends the current block.
            Err(err) => {
                failed = Some(err);
                Err(sevenz_rust2::Error::Other("stopped by decanter".into()))
            }
        }
    });
    if let Some(err) = failed {
        return Err(err);
    }
    decoded.map_err(|err| archive_error(source, err))
}
