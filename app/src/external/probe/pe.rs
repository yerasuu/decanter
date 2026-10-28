//! A Windows program's headers and the two resources it describes itself
//! with: its version strings (`VS_VERSIONINFO`) and its icon group.
//!
//! The resource tree is walked to its fixed three levels (type, name,
//! language) and no deeper, so an entry pointing back at its own directory
//! is read at most once per level rather than followed in a loop.

use super::source::{Source, u16_le, u32_le};

/// `IMAGE_FILE_EXECUTABLE_IMAGE`.
const EXECUTABLE_IMAGE: u16 = 0x0002;
/// `IMAGE_FILE_DLL`.
const DLL: u16 = 0x2000;
const PE32_MAGIC: u16 = 0x10b;
const PE32_PLUS_MAGIC: u16 = 0x20b;
/// `IMAGE_DIRECTORY_ENTRY_RESOURCE`.
const RESOURCE_DIRECTORY: usize = 2;
const SECTION_HEADER_SIZE: usize = 40;
/// The format allows 96; anything above is a corrupt or hostile header.
const MAX_SECTIONS: usize = 96;
/// Entries read from one resource directory; real programs have a few
/// dozen at most.
const MAX_ENTRIES: usize = 512;
const RT_ICON: u32 = 3;
const RT_GROUP_ICON: u32 = 14;
const RT_VERSION: u32 = 16;
/// A version resource is a few KiB.
const MAX_VERSION_SIZE: usize = 64 * 1024;
pub(super) const MAX_ICON_SIZE: usize = 2 * 1024 * 1024;
/// The sub-blocks walked inside a version resource before giving up.
const MAX_VERSION_BLOCKS: usize = 256;
/// US English, the string table preferred when a program carries several.
const ENGLISH_TABLE: &str = "0409";

pub(super) enum Header {
    Program(Program),
    Library,
    /// `MZ` without a PE image behind it: a DOS program or something else.
    Other,
}

pub(super) struct Program {
    sections: Vec<Section>,
    /// The resource directory's RVA and size; zero without resources.
    resources: (u32, u32),
}

struct Section {
    virtual_address: u32,
    virtual_size: u32,
    raw_offset: u32,
    raw_size: u32,
}

impl Section {
    /// Where `rva` lies in the file, with how many bytes of the section's
    /// raw data follow it.
    ///
    /// Bounded by the section's size in memory too: raw data past it isn't
    /// mapped, and a Godot game's appended `pck` section declares a raw size
    /// that spans the sections after it.
    fn locate(&self, rva: u32) -> Option<(u64, u32)> {
        let into = rva.checked_sub(self.virtual_address)?;
        let mapped = if self.virtual_size == 0 {
            self.raw_size
        } else {
            self.virtual_size.min(self.raw_size)
        };
        let left = mapped.checked_sub(into).filter(|&left| left > 0)?;
        Some((u64::from(self.raw_offset).checked_add(u64::from(into))?, left))
    }
}

/// What the headers after `MZ` say the file is.
pub(super) fn header(source: &mut Source) -> Header {
    read_header(source).unwrap_or(Header::Other)
}

fn read_header(source: &mut Source) -> Option<Header> {
    let dos = source.read(0, 64)?;
    let pe_offset = u64::from(u32_le(&dos, 0x3c)?);
    let coff = source.read(pe_offset, 24)?;
    if coff.get(..4)? != b"PE\0\0" {
        return None;
    }
    let characteristics = u16_le(&coff, 22)?;
    if characteristics & DLL != 0 {
        return Some(Header::Library);
    }
    if characteristics & EXECUTABLE_IMAGE == 0 {
        return None;
    }
    let section_count = usize::from(u16_le(&coff, 6)?);
    if section_count > MAX_SECTIONS {
        return None;
    }
    let optional_size = u16_le(&coff, 20)?;
    let optional_offset = pe_offset.checked_add(24)?;
    let optional = source.read(optional_offset, usize::from(optional_size))?;
    let directories: usize = match u16_le(&optional, 0)? {
        PE32_MAGIC => 96,
        PE32_PLUS_MAGIC => 112,
        _ => return None,
    };
    let directory_count = usize::try_from(u32_le(&optional, directories.checked_sub(4)?)?).ok()?;
    let resources = if directory_count > RESOURCE_DIRECTORY {
        let at = directories.checked_add(RESOURCE_DIRECTORY.checked_mul(8)?)?;
        (u32_le(&optional, at)?, u32_le(&optional, at.checked_add(4)?)?)
    } else {
        (0, 0)
    };
    let table = source.read(
        optional_offset.checked_add(u64::from(optional_size))?,
        section_count.checked_mul(SECTION_HEADER_SIZE)?,
    )?;
    let sections = table
        .as_chunks::<SECTION_HEADER_SIZE>()
        .0
        .iter()
        .map(|header| {
            Some(Section {
                virtual_size: u32_le(header, 8)?,
                virtual_address: u32_le(header, 12)?,
                raw_size: u32_le(header, 16)?,
                raw_offset: u32_le(header, 20)?,
            })
        })
        .collect::<Option<Vec<_>>>()?;
    Some(Header::Program(Program { sections, resources }))
}

struct Entry {
    /// `None` for an entry keyed by a string name.
    id: Option<u32>,
    /// Relative to the resource directory's start.
    offset: u32,
    is_directory: bool,
}

/// A resource's bytes, found through its data entry.
struct Leaf {
    rva: u32,
    size: u32,
}

impl Program {
    fn read_rva(&self, source: &mut Source, rva: u32, len: usize) -> Option<Vec<u8>> {
        let (offset, available) = self.sections.iter().find_map(|section| section.locate(rva))?;
        if usize::try_from(available).ok()? < len {
            return None;
        }
        source.read(offset, len)
    }

    fn directory(&self, source: &mut Source, offset: u32) -> Option<Vec<Entry>> {
        let (root, size) = self.resources;
        if root == 0 || offset >= size {
            return None;
        }
        let rva = root.checked_add(offset)?;
        let header = self.read_rva(source, rva, 16)?;
        let count = usize::from(u16_le(&header, 12)?)
            .checked_add(usize::from(u16_le(&header, 14)?))?
            .min(MAX_ENTRIES);
        let raw = self.read_rva(source, rva.checked_add(16)?, count.checked_mul(8)?)?;
        Some(
            raw.as_chunks::<8>()
                .0
                .iter()
                .filter_map(|entry| {
                    let name = u32_le(entry, 0)?;
                    let data = u32_le(entry, 4)?;
                    Some(Entry {
                        id: (name & 0x8000_0000 == 0).then_some(name),
                        offset: data & 0x7fff_ffff,
                        is_directory: data & 0x8000_0000 != 0,
                    })
                })
                .collect(),
        )
    }

    fn leaf(&self, source: &mut Source, offset: u32) -> Option<Leaf> {
        let (root, size) = self.resources;
        if offset >= size {
            return None;
        }
        let entry = self.read_rva(source, root.checked_add(offset)?, 16)?;
        Some(Leaf {
            rva: u32_le(&entry, 0)?,
            size: u32_le(&entry, 4)?,
        })
    }

    /// The names of `kind` with the first language of each, in the order
    /// the file lists them.
    fn resources_of(&self, source: &mut Source, kind: u32) -> Vec<(Option<u32>, Leaf)> {
        let Some(types) = self.directory(source, 0) else {
            return Vec::new();
        };
        let Some(names) = types
            .iter()
            .find(|entry| entry.is_directory && entry.id == Some(kind))
            .and_then(|entry| self.directory(source, entry.offset))
        else {
            return Vec::new();
        };
        names
            .iter()
            .filter(|name| name.is_directory)
            .filter_map(|name| {
                let languages = self.directory(source, name.offset)?;
                let language = languages.iter().find(|language| !language.is_directory)?;
                Some((name.id, self.leaf(source, language.offset)?))
            })
            .collect()
    }

    fn read_leaf(&self, source: &mut Source, leaf: &Leaf, cap: usize) -> Option<Vec<u8>> {
        let size = usize::try_from(leaf.size).ok()?;
        if size == 0 || size > cap {
            return None;
        }
        self.read_rva(source, leaf.rva, size)
    }

    /// `ProductName`, else `FileDescription`, else `InternalName`, from the
    /// English string table if there is one, else the first.
    pub(super) fn version_name(&self, source: &mut Source) -> Option<String> {
        let leaf = self.resources_of(source, RT_VERSION).into_iter().next()?.1;
        let data = self.read_leaf(source, &leaf, MAX_VERSION_SIZE)?;
        let tables = string_tables(&data);
        let table = tables
            .iter()
            .find(|table| {
                table
                    .language
                    .get(..4)
                    .is_some_and(|code| code.eq_ignore_ascii_case(ENGLISH_TABLE))
            })
            .or_else(|| tables.first())?;
        ["ProductName", "FileDescription", "InternalName"]
            .iter()
            .find_map(|key| {
                table
                    .strings
                    .iter()
                    .find(|(name, value)| name == key && !value.trim().is_empty())
            })
            .map(|(_, value)| value.clone())
    }

    /// The best image of the first icon group, which is the one Windows
    /// shows for the program.
    pub(super) fn icon(&self, source: &mut Source) -> Option<Vec<u8>> {
        let group_leaf = self.resources_of(source, RT_GROUP_ICON).into_iter().next()?.1;
        let group = self.read_leaf(source, &group_leaf, MAX_VERSION_SIZE)?;
        let best = super::icon::best_in_group(&group)?;
        let icons = self.resources_of(source, RT_ICON);
        let leaf = icons.iter().find(|(id, _)| *id == Some(u32::from(best.id)))?;
        let image = self.read_leaf(source, &leaf.1, MAX_ICON_SIZE)?;
        super::icon::to_png(&image, &best)
    }
}

struct StringTable {
    language: String,
    strings: Vec<(String, String)>,
}

/// One `VS_VERSIONINFO` block: `wLength`, `wValueLength`, `wType`, a
/// NUL-terminated UTF-16 key, then its value and its children, each
/// aligned to 4 bytes.
struct Block {
    key: String,
    /// Where the value starts, and its length in bytes.
    value: (usize, usize),
    /// Where the children start, and where the block ends.
    children: (usize, usize),
}

fn block(data: &[u8], at: usize) -> Option<Block> {
    let length = usize::from(u16_le(data, at)?);
    let value_length = usize::from(u16_le(data, at.checked_add(2)?)?);
    let is_text = u16_le(data, at.checked_add(4)?)? == 1;
    let end = at.checked_add(length)?.min(data.len());
    if length < 6 || end <= at {
        return None;
    }
    let (key, after_key) = utf16_until_nul(data, at.checked_add(6)?, end)?;
    let value_start = align4(after_key)?;
    // A text value's length counts UTF-16 units, a binary one bytes.
    let value_bytes = if is_text {
        value_length.checked_mul(2)?
    } else {
        value_length
    };
    let value_end = value_start.checked_add(value_bytes)?.min(end);
    Some(Block {
        key,
        value: (value_start, value_end.saturating_sub(value_start)),
        children: (align4(value_end)?.min(end), end),
    })
}

/// The children of the block spanning `start..end`, at most
/// [`MAX_VERSION_BLOCKS`] of them.
fn children(data: &[u8], (start, end): (usize, usize)) -> Vec<Block> {
    let mut found = Vec::new();
    let mut at = start;
    while at < end && found.len() < MAX_VERSION_BLOCKS {
        let Some(child) = block(data, at) else {
            break;
        };
        let Some(next) = align4(child.children.1) else {
            break;
        };
        if next <= at {
            break;
        }
        at = next;
        found.push(child);
    }
    found
}

fn string_tables(data: &[u8]) -> Vec<StringTable> {
    let Some(root) = block(data, 0).filter(|root| root.key == "VS_VERSION_INFO") else {
        return Vec::new();
    };
    children(data, root.children)
        .into_iter()
        .filter(|info| info.key == "StringFileInfo")
        .flat_map(|info| children(data, info.children))
        .map(|table| StringTable {
            strings: children(data, table.children)
                .into_iter()
                .filter_map(|string| {
                    let (start, len) = string.value;
                    let end = start.checked_add(len)?;
                    let (value, _) = utf16_until_nul(data, start, end).or_else(|| utf16_all(data, start, end))?;
                    Some((string.key, value))
                })
                .collect(),
            language: table.key,
        })
        .collect()
}

/// The UTF-16 text from `start` to its NUL, and where the NUL ends.
fn utf16_until_nul(data: &[u8], start: usize, end: usize) -> Option<(String, usize)> {
    let units = data
        .get(start..end)?
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| u16::from_le_bytes(*pair));
    let mut text = Vec::new();
    for unit in units {
        if unit == 0 {
            let after = start.checked_add(text.len().checked_add(1)?.checked_mul(2)?)?;
            return Some((String::from_utf16_lossy(&text), after));
        }
        text.push(unit);
    }
    None
}

/// A value that fills its span without a terminating NUL.
fn utf16_all(data: &[u8], start: usize, end: usize) -> Option<(String, usize)> {
    let units: Vec<u16> = data
        .get(start..end)?
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| u16::from_le_bytes(*pair))
        .collect();
    Some((String::from_utf16_lossy(&units), end))
}

fn align4(at: usize) -> Option<usize> {
    Some(at.checked_add(3)? & !3)
}
