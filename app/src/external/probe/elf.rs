//! A Linux program's ELF header: whether it is something to run rather
//! than a library or an object file, and whether it is an AppImage.

use super::source::Source;

const ET_EXEC: u16 = 2;
const ET_DYN: u16 = 3;
const PT_INTERP: u32 = 3;
/// Far above any real program's header count.
const MAX_PROGRAM_HEADERS: usize = 256;

pub(super) enum Elf {
    /// Something to run: `ET_EXEC`, or `ET_DYN` with an interpreter (a PIE).
    Program { is_appimage: bool },
    /// A shared library or an object file.
    Other,
}

pub(super) fn read(source: &mut Source) -> Option<Elf> {
    let header = source.read(0, 64).or_else(|| source.read(0, 52))?;
    let is_64 = match header.get(4)? {
        1 => false,
        2 => true,
        _ => return None,
    };
    let is_little = match header.get(5)? {
        1 => true,
        2 => false,
        _ => return None,
    };
    let half = |at: usize| -> Option<u16> {
        let bytes: [u8; 2] = header.get(at..at.checked_add(2)?)?.try_into().ok()?;
        Some(if is_little {
            u16::from_le_bytes(bytes)
        } else {
            u16::from_be_bytes(bytes)
        })
    };
    let word = |bytes: &[u8], at: usize| -> Option<u32> {
        let bytes: [u8; 4] = bytes.get(at..at.checked_add(4)?)?.try_into().ok()?;
        Some(if is_little {
            u32::from_le_bytes(bytes)
        } else {
            u32::from_be_bytes(bytes)
        })
    };
    let wide = |bytes: &[u8], at: usize| -> Option<u64> {
        let bytes: [u8; 8] = bytes.get(at..at.checked_add(8)?)?.try_into().ok()?;
        Some(if is_little {
            u64::from_le_bytes(bytes)
        } else {
            u64::from_be_bytes(bytes)
        })
    };
    // The AppImage specification's magic, in the ELF identification's
    // padding: `AI` and the AppImage type.
    let is_appimage = matches!(header.get(8..11), Some(b"AI\x01" | b"AI\x02"));
    let kind = half(16)?;
    if kind == ET_EXEC || kind == ET_DYN && is_appimage {
        return Some(Elf::Program { is_appimage });
    }
    if kind != ET_DYN {
        return Some(Elf::Other);
    }
    let (table, entry_size, count) = if is_64 {
        (wide(&header, 32)?, half(54)?, half(56)?)
    } else {
        (u64::from(word(&header, 28)?), half(42)?, half(44)?)
    };
    let count = usize::from(count).min(MAX_PROGRAM_HEADERS);
    let entry_size = usize::from(entry_size);
    if entry_size < 4 {
        return Some(Elf::Other);
    }
    let headers = source.read(table, entry_size.checked_mul(count)?)?;
    let has_interpreter = headers
        .chunks_exact(entry_size)
        .any(|entry| word(entry, 0) == Some(PT_INTERP));
    Some(if has_interpreter {
        Elf::Program { is_appimage }
    } else {
        Elf::Other
    })
}
