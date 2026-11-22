//! RAR's block headers, read here before UnRAR decodes anything because the `unrar` crate does not
//! report a file's packed size, dictionary or link record. The walk follows UnRAR's own
//! (`arcread.cpp` of the UnRAR 7.1.0 source `unrar_sys` builds): the same block sizes, fields
//! read as zero past a header's end, and the walk ending where UnRAR's ends. Where the two could
//! still disagree this side refuses more, and `super` checks the files it found against UnRAR's.
//! Formats: [RAR 5.0 technical note](https://www.rarlab.com/technote.htm); RAR 1.5 to 4.x only
//! as that source reads it.

use std::fs;
use std::io::{self, Read as _, Seek as _, SeekFrom};
use std::path::Path;

use super::super::MAX_DECODER_MEMORY;
use crate::{Error, MAX_ENTRIES, Result};

const SIGNATURE_4: &[u8] = b"Rar!\x1A\x07\x00";
const SIGNATURE_5: &[u8] = b"Rar!\x1A\x07\x01\x00";
/// A file's header and the service headers that follow it (streams, ACL, owner); an archive with
/// more blocks than this is none a RAR tool writes.
const MAX_BLOCKS: usize = MAX_ENTRIES.saturating_mul(4);
/// RAR 5.0 keeps a header's size to three bytes of vint, as UnRAR enforces.
const MAX_HEADER_5: usize = 1 << 21;
const FIRST_READ_5: usize = 7;

const HEAD5_MAIN: u64 = 1;
const HEAD5_FILE: u64 = 2;
const HEAD5_SERVICE: u64 = 3;
const HEAD5_CRYPT: u64 = 4;
const HEAD5_END: u64 = 5;
const HFL_EXTRA: u64 = 0x01;
const HFL_DATA: u64 = 0x02;
const HFL_SPLIT: u64 = 0x08 | 0x10;
const MHFL_VOLUME: u64 = 0x01;
const MHFL_VOLNUMBER: u64 = 0x02;
const MHFL_SOLID: u64 = 0x04;
const MHEXTRA_LOCATOR: u64 = 0x01;
const MHEXTRA_LOCATOR_QLIST: u64 = 0x01;
const FHFL_DIRECTORY: u64 = 0x01;
const FHFL_UTIME: u64 = 0x02;
const FHFL_CRC32: u64 = 0x04;
const FHFL_UNPUNKNOWN: u64 = 0x08;
const FCI_SOLID: u64 = 0x40;
const FHEXTRA_CRYPT: u64 = 0x01;
const FHEXTRA_REDIR: u64 = 0x05;
const HOST5_WINDOWS: u64 = 0;
const HOST5_UNIX: u64 = 1;

const HEAD4_MAIN: u8 = 0x73;
const HEAD4_FILE: u8 = 0x74;
const HEAD4_CMT: u8 = 0x75;
const HEAD4_OLDSERVICE: u8 = 0x77;
const HEAD4_PROTECT: u8 = 0x78;
const HEAD4_SERVICE: u8 = 0x7A;
const HEAD4_END: u8 = 0x7B;
const SIZEOF_SHORTBLOCKHEAD_4: usize = 7;
const SIZEOF_COMMHEAD_4: usize = 13;
const MHD_VOLUME: u16 = 0x0001;
const MHD_COMMENT: u16 = 0x0002;
const MHD_SOLID: u16 = 0x0008;
const MHD_PASSWORD: u16 = 0x0080;
const LHD_SPLIT: u16 = 0x0001 | 0x0002;
const LHD_PASSWORD: u16 = 0x0004;
const LHD_SOLID: u16 = 0x0010;
const LHD_WINDOWMASK: u16 = 0x00E0;
const LHD_LARGE: u16 = 0x0100;
const LONG_BLOCK: u16 = 0x8000;
const HOST4_UNIX: u8 = 3;
const HOST4_BEOS: u8 = 5;

/// [File attribute constants](https://learn.microsoft.com/en-us/windows/win32/fileio/file-attribute-constants).
const FILE_ATTRIBUTE_DIRECTORY: u64 = 0x10;
const FILE_ATTRIBUTE_REPARSE_POINT: u64 = 0x400;
const S_IFMT: u64 = 0o170_000;
const S_IFREG: u64 = 0o100_000;
const S_IFDIR: u64 = 0o040_000;

/// A file header as UnRAR will list it.
pub(super) struct Header {
    pub(super) is_dir: bool,
    pub(super) size: u64,
    pub(super) packed: u64,
}

pub(super) struct Headers {
    pub(super) files: Vec<Header>,
    /// Whether the files decode as one stream, so their ratio is only meaningful together.
    pub(super) solid: bool,
}

/// Refuses an encrypted or multi-volume archive, a link or special file, a dictionary above
/// [`MAX_DECODER_MEMORY`] and a file of unknown size, which UnRAR would only meet while decoding.
pub(super) fn read(source: &Path) -> Result<Headers> {
    let mut file = fs::File::open(source).map_err(Error::io(source))?;
    let signature = read_at(&mut file, 0, SIGNATURE_5.len()).map_err(Error::io(source))?;
    let mut walk = Walk {
        source,
        file,
        headers: Headers {
            files: Vec::new(),
            solid: false,
        },
        blocks: 0,
        seen_main: false,
    };
    if signature.starts_with(SIGNATURE_5) {
        walk.rar5()?;
    } else if signature.starts_with(SIGNATURE_4) {
        walk.rar4()?;
    } else {
        return Err(Error::UnknownFormat);
    }
    Ok(walk.headers)
}

struct Walk<'source> {
    source: &'source Path,
    file: fs::File,
    headers: Headers,
    blocks: usize,
    seen_main: bool,
}

impl Walk<'_> {
    fn read_at(&mut self, pos: u64, len: usize) -> Result<Vec<u8>> {
        read_at(&mut self.file, pos, len).map_err(Error::io(self.source))
    }

    const fn count_block(&mut self) -> Result<()> {
        self.blocks = self.blocks.saturating_add(1);
        if self.blocks > MAX_BLOCKS || self.headers.files.len() > MAX_ENTRIES {
            return Err(Error::TooManyEntries);
        }
        Ok(())
    }

    fn main_header(&mut self) -> Result<()> {
        // A later main header makes UnRAR load another quick-open copy, which isn't compared.
        if self.seen_main {
            return Err(Error::Archive("it has two main headers".to_owned()));
        }
        self.seen_main = true;
        Ok(())
    }

    fn rar5(&mut self) -> Result<()> {
        let mut pos = u64::try_from(SIGNATURE_5.len()).unwrap_or(u64::MAX);
        let mut quick_open = Vec::new();
        loop {
            self.count_block()?;
            let first = self.read_at(pos, FIRST_READ_5)?;
            if first.len() < FIRST_READ_5 {
                break;
            }
            let mut prefix = Raw::new(&first);
            prefix.skip(4);
            let size_bytes = vint_size(first.get(4..).unwrap_or_default());
            let block_size = prefix.vint();
            let Some(header_size) = usize::try_from(block_size)
                .ok()
                .filter(|&size| size_bytes != 0 && size != 0 && size <= MAX_HEADER_5)
                .map(|size| size.saturating_add(size_bytes).saturating_add(4))
                .filter(|&size| size >= FIRST_READ_5)
            else {
                break;
            };
            let header = self.read_at(pos, header_size)?;
            if header.len() < header_size {
                break;
            }
            let mut fields = Raw::new(&header);
            fields.skip(size_bytes.saturating_add(4));
            let kind = fields.vint();
            let flags = fields.vint();
            let extra_size = if flags & HFL_EXTRA == 0 { 0 } else { fields.vint() };
            if usize::try_from(extra_size).map_or(true, |extra| extra >= header_size) {
                break;
            }
            let data_size = if flags & HFL_DATA == 0 { 0 } else { fields.vint() };
            let Some(next) = next_block(pos, header_size, data_size) else {
                break;
            };
            let extra = header
                .get(
                    header
                        .len()
                        .saturating_sub(usize::try_from(extra_size).unwrap_or(usize::MAX))..,
                )
                .unwrap_or_default();
            match kind {
                HEAD5_CRYPT => return Err(Error::Encrypted),
                HEAD5_MAIN => {
                    self.main_header()?;
                    let archive_flags = fields.vint();
                    if archive_flags & (MHFL_VOLUME | MHFL_VOLNUMBER) != 0 {
                        return Err(Error::MultiVolume);
                    }
                    self.headers.solid |= archive_flags & MHFL_SOLID != 0;
                    quick_open.extend(quick_open_offsets(extra).filter_map(|offset| pos.checked_add(offset)));
                }
                HEAD5_FILE | HEAD5_SERVICE => {
                    let (file, solid) = file5(&mut fields, flags, data_size, extra)?;
                    if kind == HEAD5_FILE {
                        self.headers.solid |= solid;
                        self.headers.files.push(file);
                    }
                }
                HEAD5_END => break,
                _ => {}
            }
            pos = next;
        }
        for at in quick_open {
            self.compare_quick_open(at)?;
        }
        Ok(())
    }

    /// UnRAR reads headers from the quick-open copy wherever it holds them, so the copy must be
    /// what the walk above read: any record differing from the bytes at its place refuses the
    /// archive rather than let UnRAR list other files than were checked.
    fn compare_quick_open(&mut self, at: u64) -> Result<()> {
        let mismatch = || Error::Archive("its quick-open copy of the headers doesn't match them".to_owned());
        let first = self.read_at(at, FIRST_READ_5)?;
        let size_bytes = vint_size(first.get(4..).unwrap_or_default());
        let mut prefix = Raw::new(&first);
        prefix.skip(4);
        let Some(header_size) = usize::try_from(prefix.vint())
            .ok()
            .filter(|&size| size_bytes != 0 && size != 0 && size <= MAX_HEADER_5)
            .map(|size| size.saturating_add(size_bytes).saturating_add(4))
            .filter(|&size| size >= FIRST_READ_5)
        else {
            return Ok(());
        };
        let header = self.read_at(at, header_size)?;
        let mut fields = Raw::new(&header);
        fields.skip(size_bytes.saturating_add(4));
        if fields.vint() != HEAD5_SERVICE {
            return Ok(());
        }
        let flags = fields.vint();
        if flags & HFL_EXTRA != 0 {
            fields.vint();
        }
        if flags & HFL_DATA != 0 {
            fields.vint();
        }
        fields.vint();
        // UnRAR takes the unpacked size as the copy's length, quick-open data being stored.
        let length = fields.vint();
        let start = at.saturating_add(u64::try_from(header_size).unwrap_or(u64::MAX));
        // A handle of its own, since the comparison below moves the walk's.
        let copy = fs::File::open(self.source).map_err(Error::io(self.source))?;
        let mut data = io::BufReader::new(copy);
        data.seek(SeekFrom::Start(start)).map_err(Error::io(self.source))?;
        let mut data = data.take(length);
        let mut records = 0_usize;
        loop {
            records = records.saturating_add(1);
            if records > MAX_BLOCKS {
                return Err(Error::TooManyEntries);
            }
            let mut record = vec![0_u8; FIRST_READ_5];
            if read_full(&mut data, &mut record).map_err(Error::io(self.source))? < FIRST_READ_5 {
                return Ok(());
            }
            let record_size_bytes = vint_size(record.get(4..).unwrap_or_default());
            let mut record_prefix = Raw::new(&record);
            record_prefix.skip(4);
            let block_size = usize::try_from(record_prefix.vint()).unwrap_or(usize::MAX);
            let Some(rest) = block_size
                .checked_add(record_size_bytes)
                .and_then(|size| size.checked_add(4))
                .and_then(|size| size.checked_sub(FIRST_READ_5))
                .filter(|&rest| record_size_bytes != 0 && block_size != 0 && rest <= MAX_HEADER_5)
            else {
                return Ok(());
            };
            record.resize(FIRST_READ_5.saturating_add(rest), 0);
            let tail = record.get_mut(FIRST_READ_5..).unwrap_or_default();
            if read_full(&mut data, tail).map_err(Error::io(self.source))? < rest {
                return Ok(());
            }
            let mut record_fields = Raw::new(&record);
            record_fields.skip(record_size_bytes.saturating_add(4));
            record_fields.vint();
            let offset = record_fields.vint();
            let Ok(cached_size) = usize::try_from(record_fields.vint()) else {
                continue;
            };
            if cached_size > MAX_HEADER_5 {
                continue;
            }
            let mut cached = record_fields.bytes(cached_size).to_vec();
            cached.resize(cached_size, 0);
            let place = at.checked_sub(offset).ok_or_else(mismatch)?;
            if self.read_at(place, cached_size)? != cached {
                return Err(mismatch());
            }
        }
    }

    fn rar4(&mut self) -> Result<()> {
        let mut pos = u64::try_from(SIGNATURE_4.len()).unwrap_or(u64::MAX);
        loop {
            self.count_block()?;
            let short = self.read_at(pos, SIZEOF_SHORTBLOCKHEAD_4)?;
            if short.len() < SIZEOF_SHORTBLOCKHEAD_4 {
                break;
            }
            let mut prefix = Raw::new(&short);
            prefix.skip(2);
            let kind = prefix.byte();
            let flags = prefix.u16();
            let head_size = usize::from(prefix.u16());
            if head_size < SIZEOF_SHORTBLOCKHEAD_4 {
                break;
            }
            // Only the fixed part of a comment, as UnRAR, since the rest is the comment's own data.
            let read_size = if kind == HEAD4_CMT || (kind == HEAD4_MAIN && flags & MHD_COMMENT != 0) {
                SIZEOF_COMMHEAD_4
            } else {
                head_size
            };
            let header = self.read_at(pos, read_size)?;
            let mut fields = Raw::new(&header);
            fields.skip(SIZEOF_SHORTBLOCKHEAD_4);
            let data_size = match kind {
                HEAD4_MAIN => {
                    self.main_header()?;
                    if flags & MHD_PASSWORD != 0 {
                        return Err(Error::Encrypted);
                    }
                    if flags & MHD_VOLUME != 0 {
                        return Err(Error::MultiVolume);
                    }
                    self.headers.solid |= flags & MHD_SOLID != 0;
                    0
                }
                HEAD4_FILE | HEAD4_SERVICE => {
                    let file = file4(&mut fields, flags)?;
                    let packed = file.packed;
                    if kind == HEAD4_FILE {
                        self.headers.solid |= flags & LHD_SOLID != 0;
                        self.headers.files.push(file);
                    }
                    packed
                }
                HEAD4_END => break,
                HEAD4_CMT => 0,
                HEAD4_PROTECT | HEAD4_OLDSERVICE => u64::from(fields.u32()),
                _ if flags & LONG_BLOCK != 0 => u64::from(fields.u32()),
                _ => 0,
            };
            let Some(next) = next_block(pos, head_size, data_size) else {
                break;
            };
            pos = next;
        }
        Ok(())
    }
}

/// A RAR 5.0 file or service header's fields after its block's, and whether it continues a solid
/// stream.
fn file5(raw: &mut Raw<'_>, block_flags: u64, packed: u64, extra: &[u8]) -> Result<(Header, bool)> {
    let file_flags = raw.vint();
    let size = raw.vint();
    let attributes = raw.vint();
    if file_flags & FHFL_UTIME != 0 {
        raw.skip(4);
    }
    if file_flags & FHFL_CRC32 != 0 {
        raw.skip(4);
    }
    let compression = raw.vint();
    let host = raw.vint();
    let name_size = usize::try_from(raw.vint()).unwrap_or(usize::MAX);
    let name = String::from_utf8_lossy(raw.bytes(name_size)).into_owned();
    let is_dir = file_flags & FHFL_DIRECTORY != 0;
    if block_flags & HFL_SPLIT != 0 {
        return Err(Error::MultiVolume);
    }
    if file_flags & FHFL_UNPUNKNOWN != 0 {
        return Err(Error::Archive(format!("{name:?} doesn't record its size")));
    }
    for (kind, _) in extra_records(extra) {
        match kind {
            FHEXTRA_CRYPT => return Err(Error::Encrypted),
            FHEXTRA_REDIR => return Err(Error::LinkEntry(name)),
            _ => {}
        }
    }
    let is_special = match host {
        HOST5_WINDOWS => attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0,
        HOST5_UNIX => !matches!(attributes & S_IFMT, 0 | S_IFREG | S_IFDIR),
        _ => false,
    };
    if is_special {
        return Err(Error::LinkEntry(name));
    }
    let method = (compression >> 7_u32) & 7;
    if !is_dir && method != 0 {
        check_dictionary(&name, dictionary5(compression).ok_or_else(|| unknown_version(&name))?)?;
    }
    Ok((Header { is_dir, size, packed }, compression & FCI_SOLID != 0))
}

fn file4(raw: &mut Raw<'_>, flags: u16) -> Result<Header> {
    let packed_low = raw.u32();
    let size_low = raw.u32();
    let host = raw.byte();
    raw.skip(8);
    let version = raw.byte();
    let method = raw.byte();
    let name_size = usize::from(raw.u16());
    let attributes = u64::from(raw.u32());
    let (packed_high, size_high) = if flags & LHD_LARGE == 0 {
        (0, 0)
    } else {
        (raw.u32(), raw.u32())
    };
    let name = raw.bytes(name_size);
    let name = String::from_utf8_lossy(name.split(|&byte| byte == 0).next().unwrap_or_default()).into_owned();
    if flags & LHD_SPLIT != 0 {
        return Err(Error::MultiVolume);
    }
    if flags & LHD_PASSWORD != 0 {
        return Err(Error::Encrypted);
    }
    let unknown_size = size_low == u32::MAX && (flags & LHD_LARGE == 0 || size_high == u32::MAX);
    if unknown_size {
        return Err(Error::Archive(format!("{name:?} doesn't record its size")));
    }
    let is_dir =
        flags & LHD_WINDOWMASK == LHD_WINDOWMASK || (version < 20 && attributes & FILE_ATTRIBUTE_DIRECTORY != 0);
    let is_special = if host == HOST4_UNIX || host == HOST4_BEOS {
        !matches!(attributes & S_IFMT, 0 | S_IFREG | S_IFDIR)
    } else {
        attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0
    };
    if is_special {
        return Err(Error::LinkEntry(name));
    }
    // b'0' is "stored", which allocates no window.
    if !is_dir && method != b'0' {
        check_dictionary(&name, 0x1_0000_u64 << u32::from((flags & LHD_WINDOWMASK) >> 5_u32))?;
    }
    Ok(Header {
        is_dir,
        size: (u64::from(size_high) << 32_u32) | u64::from(size_low),
        packed: (u64::from(packed_high) << 32_u32) | u64::from(packed_low),
    })
}

/// What a RAR 5.0 or 7.0 file needs for its window, from its compression info as UnRAR computes
/// it; `None` for an algorithm version it has no decoder for.
const fn dictionary5(compression: u64) -> Option<u64> {
    let base = 0x2_0000_u64;
    match compression & 0x3F {
        0 => Some(base << ((compression >> 10_u32) & 0x0F)),
        1 => {
            let size = base << ((compression >> 10_u32) & 0x1F);
            // The fraction is in 32nds of the power of two.
            Some(size.saturating_add((size >> 5_u32).saturating_mul((compression >> 15_u32) & 0x1F)))
        }
        _ => None,
    }
}

fn check_dictionary(name: &str, dictionary: u64) -> Result<()> {
    if dictionary > MAX_DECODER_MEMORY {
        tracing::info!(name, dictionary, "RAR entry refused for its dictionary size");
        return Err(Error::Memory(dictionary >> 20_u32));
    }
    Ok(())
}

fn unknown_version(name: &str) -> Error {
    Error::Archive(format!("{name:?} uses a RAR compression version decanter can't decode"))
}

/// Where the block after one at `pos` starts, or `None` where UnRAR stops the walk as broken.
fn next_block(pos: u64, header_size: usize, data_size: u64) -> Option<u64> {
    pos.checked_add(u64::try_from(header_size).ok()?)?
        .checked_add(data_size)
        .filter(|&next| next > pos && i64::try_from(next).is_ok())
}

/// The locator records' quick-open offsets, relative to the main header.
fn quick_open_offsets(extra: &[u8]) -> impl Iterator<Item = u64> + '_ {
    extra_records(extra).filter_map(|(kind, body)| {
        if kind != MHEXTRA_LOCATOR {
            return None;
        }
        let mut raw = Raw::new(body);
        let flags = raw.vint();
        let offset = if flags & MHEXTRA_LOCATOR_QLIST == 0 {
            0
        } else {
            raw.vint()
        };
        (offset != 0).then_some(offset)
    })
}

/// A RAR 5.0 extra area as `(type, body)` records, ending where UnRAR stops reading it.
fn extra_records(extra: &[u8]) -> impl Iterator<Item = (u64, &[u8])> + '_ {
    let mut raw = Raw::new(extra);
    core::iter::from_fn(move || {
        if raw.left() < 2 {
            return None;
        }
        let size = usize::try_from(raw.vint())
            .ok()
            .filter(|&size| size != 0 && size <= raw.left())?;
        let end = raw.pos.saturating_add(size);
        let kind = raw.vint();
        let body = raw.data.get(raw.pos..end)?;
        raw.pos = end;
        Some((kind, body))
    })
}

/// How many bytes the vint at the start of `bytes` takes, 0 when it doesn't end inside them.
fn vint_size(bytes: &[u8]) -> usize {
    bytes
        .iter()
        .position(|&byte| byte & 0x80 == 0)
        .map_or(0, |last| last.saturating_add(1))
}

/// A header's bytes read as UnRAR's `RawRead` does: a field past the end reads as zero.
struct Raw<'data> {
    data: &'data [u8],
    pos: usize,
}

impl<'data> Raw<'data> {
    const fn new(data: &'data [u8]) -> Self {
        Self { data, pos: 0 }
    }

    const fn left(&self) -> usize {
        self.data.len().saturating_sub(self.pos)
    }

    const fn skip(&mut self, count: usize) {
        self.pos = self.pos.saturating_add(count);
    }

    fn bytes(&mut self, count: usize) -> &'data [u8] {
        let start = self.pos.min(self.data.len());
        let end = start.saturating_add(count).min(self.data.len());
        self.pos = end;
        self.data.get(start..end).unwrap_or_default()
    }

    fn fixed<const N: usize>(&mut self) -> Option<[u8; N]> {
        let bytes = self
            .data
            .get(self.pos..self.pos.saturating_add(N))
            .and_then(|bytes| <[u8; N]>::try_from(bytes).ok())?;
        self.pos = self.pos.saturating_add(N);
        Some(bytes)
    }

    fn byte(&mut self) -> u8 {
        self.fixed::<1>().map_or(0, |[byte]| byte)
    }

    fn u16(&mut self) -> u16 {
        self.fixed().map_or(0, u16::from_le_bytes)
    }

    fn u32(&mut self) -> u32 {
        self.fixed().map_or(0, u32::from_le_bytes)
    }

    fn vint(&mut self) -> u64 {
        let mut value = 0_u64;
        let mut shift = 0_u32;
        while shift < 64 {
            let Some(&byte) = self.data.get(self.pos) else {
                break;
            };
            self.pos = self.pos.saturating_add(1);
            value = value.wrapping_add(u64::from(byte & 0x7F).wrapping_shl(shift));
            if byte & 0x80 == 0 {
                return value;
            }
            shift = shift.saturating_add(7);
        }
        0
    }
}

fn read_at(file: &mut fs::File, pos: u64, len: usize) -> io::Result<Vec<u8>> {
    file.seek(SeekFrom::Start(pos))?;
    let mut buffer = vec![0_u8; len];
    let read = read_full(file, &mut buffer)?;
    buffer.truncate(read);
    Ok(buffer)
}

/// Reads until `buffer` is full or the input ends, returning how much was read.
fn read_full(reader: &mut impl io::Read, buffer: &mut [u8]) -> io::Result<usize> {
    let mut read = 0_usize;
    while let Some(rest) = buffer.get_mut(read..).filter(|rest| !rest.is_empty()) {
        match reader.read(rest) {
            Ok(0) => break,
            Ok(count) => read = read.saturating_add(count),
            Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
            Err(err) => return Err(err),
        }
    }
    Ok(read)
}
