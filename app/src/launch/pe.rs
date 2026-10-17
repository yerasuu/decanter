//! The DLLs a Windows executable imports, read from its PE import table.
//!
//! The executable is a game's file, so every offset in it is checked
//! against what was actually read before it is followed.

use std::fs::File;
use std::io::{self, Read as _, Seek as _, SeekFrom};
use std::path::Path;

/// `IMAGE_DIRECTORY_ENTRY_IMPORT`.
const IMPORT_DIRECTORY: usize = 1;
const PE32_MAGIC: u16 = 0x10b;
const PE32_PLUS_MAGIC: u16 = 0x20b;
const SECTION_HEADER_SIZE: usize = 40;
const IMPORT_DESCRIPTOR_SIZE: u8 = 20;
/// The format allows 96; anything above is a corrupt or hostile header.
const MAX_SECTIONS: usize = 96;
/// Far above what any real program imports, so a table missing its
/// terminator can't keep the reader going.
const MAX_IMPORTS: usize = 1024;
/// Longer than any DLL name Windows itself would load.
const MAX_NAME: usize = 256;

/// The names of the DLLs `executable` imports, as the file spells them.
///
/// Delay-loaded DLLs and those loaded at run time are not listed.
///
/// # Errors
///
/// Returns an error if the file can't be read, and `InvalidData` if it
/// isn't a PE image or its headers point outside of it.
pub(super) fn imported_dlls(executable: &Path) -> io::Result<Vec<String>> {
    let mut file = File::open(executable)?;
    let dos = read_at(&mut file, 0, 64)?;
    if dos.get(..2) != Some(b"MZ".as_slice()) {
        return Err(invalid("no MZ signature"));
    }
    let pe_offset = u64::from(u32_at(&dos, 0x3c)?);
    let coff = read_at(&mut file, pe_offset, 24)?;
    if coff.get(..4) != Some(b"PE\0\0".as_slice()) {
        return Err(invalid("no PE signature"));
    }
    let section_count = usize::from(u16_at(&coff, 6)?);
    if section_count > MAX_SECTIONS {
        return Err(invalid("too many sections"));
    }
    let optional_size = u16_at(&coff, 20)?;
    let optional_offset = pe_offset
        .checked_add(24)
        .ok_or_else(|| invalid("header offset overflows"))?;
    let optional = read_at(&mut file, optional_offset, usize::from(optional_size))?;
    let directories: usize = match u16_at(&optional, 0)? {
        PE32_MAGIC => 96,
        PE32_PLUS_MAGIC => 112,
        _ => return Err(invalid("unknown optional header magic")),
    };
    let directory_count = usize::try_from(u32_at(&optional, directories.saturating_sub(4))?).unwrap_or(usize::MAX);
    if directory_count <= IMPORT_DIRECTORY {
        return Ok(Vec::new());
    }
    let import_rva = u32_at(
        &optional,
        directories.saturating_add(IMPORT_DIRECTORY.saturating_mul(8)),
    )?;
    if import_rva == 0 {
        return Ok(Vec::new());
    }

    let sections_offset = optional_offset
        .checked_add(u64::from(optional_size))
        .ok_or_else(|| invalid("section table offset overflows"))?;
    let table = read_at(
        &mut file,
        sections_offset,
        section_count.saturating_mul(SECTION_HEADER_SIZE),
    )?;
    let sections = table
        .as_chunks::<SECTION_HEADER_SIZE>()
        .0
        .iter()
        .map(Section::parse)
        .collect::<io::Result<Vec<_>>>()?;

    let mut names = Vec::new();
    let mut descriptor_rva = import_rva;
    for _ in 0..MAX_IMPORTS {
        let descriptor = read_rva(
            &mut file,
            &sections,
            descriptor_rva,
            usize::from(IMPORT_DESCRIPTOR_SIZE),
        )?;
        let name_rva = u32_at(&descriptor, 12)?;
        if name_rva == 0 {
            return Ok(names);
        }
        names.push(read_name(&mut file, &sections, name_rva)?);
        descriptor_rva = descriptor_rva
            .checked_add(u32::from(IMPORT_DESCRIPTOR_SIZE))
            .ok_or_else(|| invalid("import table runs past the address space"))?;
    }
    Err(invalid("import table has no terminator"))
}

struct Section {
    virtual_address: u32,
    raw_offset: u32,
    raw_size: u32,
}

impl Section {
    fn parse(header: &[u8; SECTION_HEADER_SIZE]) -> io::Result<Self> {
        Ok(Self {
            virtual_address: u32_at(header, 12)?,
            raw_size: u32_at(header, 16)?,
            raw_offset: u32_at(header, 20)?,
        })
    }

    /// Where `rva` lies in the file, with how many bytes of the section's
    /// raw data follow it.
    fn locate(&self, rva: u32) -> Option<(u64, u32)> {
        let into = rva.checked_sub(self.virtual_address)?;
        if into >= self.raw_size {
            return None;
        }
        Some((
            u64::from(self.raw_offset).checked_add(u64::from(into))?,
            self.raw_size.checked_sub(into)?,
        ))
    }
}

fn read_rva(file: &mut File, sections: &[Section], rva: u32, len: usize) -> io::Result<Vec<u8>> {
    let (offset, available) = sections
        .iter()
        .find_map(|section| section.locate(rva))
        .ok_or_else(|| invalid("address outside every section"))?;
    if usize::try_from(available).unwrap_or(usize::MAX) < len {
        return Err(invalid("data runs past its section"));
    }
    read_at(file, offset, len)
}

fn read_name(file: &mut File, sections: &[Section], rva: u32) -> io::Result<String> {
    let (offset, available) = sections
        .iter()
        .find_map(|section| section.locate(rva))
        .ok_or_else(|| invalid("name outside every section"))?;
    let len = usize::try_from(available).unwrap_or(usize::MAX).min(MAX_NAME);
    let bytes = read_up_to(file, offset, len)?;
    let end = bytes
        .iter()
        .position(|&byte| byte == 0)
        .ok_or_else(|| invalid("unterminated DLL name"))?;
    let name = bytes.get(..end).ok_or_else(|| invalid("unterminated DLL name"))?;
    if !name.is_ascii() {
        return Err(invalid("DLL name is not ASCII"));
    }
    Ok(String::from_utf8_lossy(name).into_owned())
}

fn read_at(file: &mut File, offset: u64, len: usize) -> io::Result<Vec<u8>> {
    let bytes = read_up_to(file, offset, len)?;
    if bytes.len() < len {
        return Err(invalid("header runs past the end of the file"));
    }
    Ok(bytes)
}

/// At most `len` bytes from `offset`, fewer at the end of the file.
fn read_up_to(file: &mut File, offset: u64, len: usize) -> io::Result<Vec<u8>> {
    file.seek(SeekFrom::Start(offset))?;
    let mut bytes = Vec::with_capacity(len);
    file.take(u64::try_from(len).unwrap_or(u64::MAX))
        .read_to_end(&mut bytes)?;
    Ok(bytes)
}

fn u16_at(bytes: &[u8], at: usize) -> io::Result<u16> {
    bytes
        .get(at..at.saturating_add(2))
        .and_then(|field| field.try_into().ok())
        .map(u16::from_le_bytes)
        .ok_or_else(|| invalid("header field past its end"))
}

fn u32_at(bytes: &[u8], at: usize) -> io::Result<u32> {
    bytes
        .get(at..at.saturating_add(4))
        .and_then(|field| field.try_into().ok())
        .map(u32::from_le_bytes)
        .ok_or_else(|| invalid("header field past its end"))
}

fn invalid(reason: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, reason)
}
