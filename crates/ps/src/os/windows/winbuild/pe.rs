//! The DLL names a Windows program or DLL imports, read from its own PE headers rather than by running
//! a tool on it: its import table and its delay-load import table ([PE format]). Every offset is
//! checked against the file, so a malformed file reads as `None`, never out of bounds.
//!
//! [PE format]: https://learn.microsoft.com/en-us/windows/win32/debug/pe-format

/// Past this, a table is taken as malformed rather than read on.
const MAX_IMPORTS: usize = 4096;
const MAX_NAME_BYTES: usize = 256;
/// The PE format allows 96 sections; more reads as malformed.
const MAX_SECTIONS: usize = 96;
const IMPORT_DIRECTORY: usize = 1;
const DELAY_IMPORT_DIRECTORY: usize = 13;

/// The DLLs `image` imports, as its tables spell them, or `None` when it isn't a PE image the
/// tables can be read from.
pub(super) fn imports(image: &[u8]) -> Option<Vec<String>> {
    if image.get(..2)? != b"MZ" {
        return None;
    }
    let pe = usize::try_from(u32_at(image, 0x3c)?).ok()?;
    if image.get(pe..pe.checked_add(4)?)? != b"PE\0\0" {
        return None;
    }
    let coff = pe + 4;
    let sections = usize::from(u16_at(image, coff + 2)?);
    if sections > MAX_SECTIONS {
        return None;
    }
    let optional_size = usize::from(u16_at(image, coff + 16)?);
    let optional = coff + 20;
    let (count_at, directories) = match u16_at(image, optional)? {
        0x20b => (optional + 108, optional + 112),
        0x10b => (optional + 92, optional + 96),
        _ => return None,
    };
    let directory_count = usize::try_from(u32_at(image, count_at)?).ok()?;
    let table = optional.checked_add(optional_size)?;
    let headers = Headers { image, table, sections };
    let mut names = Vec::new();
    if directory_count > IMPORT_DIRECTORY {
        let rva = u32_at(image, directories + 8 * IMPORT_DIRECTORY)?;
        if rva != 0 {
            names.extend(headers.names(rva, 20, 12)?);
        }
    }
    if directory_count > DELAY_IMPORT_DIRECTORY {
        let rva = u32_at(image, directories + 8 * DELAY_IMPORT_DIRECTORY)?;
        if rva != 0 {
            names.extend(headers.names(rva, 32, 4)?);
        }
    }
    Some(names)
}

struct Headers<'image> {
    image: &'image [u8],
    /// Where the section table starts.
    table: usize,
    sections: usize,
}

impl Headers<'_> {
    /// The names of a table of `stride`-byte descriptors at `rva` that ends with a zero name,
    /// each name's RVA at `name_at` in its descriptor.
    fn names(&self, rva: u32, stride: usize, name_at: usize) -> Option<Vec<String>> {
        let start = self.offset(rva)?;
        let mut names = Vec::new();
        for index in 0..MAX_IMPORTS {
            let descriptor = start.checked_add(index.checked_mul(stride)?)?;
            let name_rva = u32_at(self.image, descriptor + name_at)?;
            if name_rva == 0 {
                return Some(names);
            }
            names.push(self.c_string(self.offset(name_rva)?)?);
        }
        None
    }

    /// The file offset of `rva`, inside the raw data of the section that maps it.
    fn offset(&self, rva: u32) -> Option<usize> {
        let rva = usize::try_from(rva).ok()?;
        (0..self.sections).find_map(|index| {
            let header = self.table.checked_add(index.checked_mul(40)?)?;
            let address = usize::try_from(u32_at(self.image, header + 12)?).ok()?;
            let raw_size = usize::try_from(u32_at(self.image, header + 16)?).ok()?;
            let raw = usize::try_from(u32_at(self.image, header + 20)?).ok()?;
            let within = rva.checked_sub(address)?;
            (within < raw_size)
                .then(|| raw.checked_add(within))
                .flatten()
                .filter(|&offset| offset < self.image.len())
        })
    }

    fn c_string(&self, offset: usize) -> Option<String> {
        let rest = self.image.get(offset..)?;
        let rest = rest.get(..rest.len().min(MAX_NAME_BYTES))?;
        let end = rest.iter().position(|&byte| byte == 0)?;
        let name = rest.get(..end)?;
        (!name.is_empty() && name.iter().all(u8::is_ascii_graphic)).then(|| String::from_utf8_lossy(name).into_owned())
    }
}

fn u16_at(image: &[u8], at: usize) -> Option<u16> {
    let bytes = image.get(at..at.checked_add(2)?)?;
    Some(u16::from_le_bytes([*bytes.first()?, *bytes.get(1)?]))
}

fn u32_at(image: &[u8], at: usize) -> Option<u32> {
    let bytes = image.get(at..at.checked_add(4)?)?;
    Some(u32::from_le_bytes([
        *bytes.first()?,
        *bytes.get(1)?,
        *bytes.get(2)?,
        *bytes.get(3)?,
    ]))
}
