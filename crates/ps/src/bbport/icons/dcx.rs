//! FromSoftware's DCX container, as Bloodborne ships it: a fixed 0x4C-byte
//! header and one zlib (`DFLT`) stream.

use std::io::{self, Read as _, Write as _};

use flate2::Compression;
use flate2::read::ZlibDecoder;
use flate2::write::ZlibEncoder;

const HEADER: usize = 0x4C;

/// The header, kept to write the file back the way the game expects it,
/// and the contents.
pub struct Dcx {
    header: Vec<u8>,
    data: Vec<u8>,
}

fn invalid(what: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, what.to_owned())
}

fn be_u32(bytes: &[u8], at: usize) -> io::Result<usize> {
    bytes
        .get(at..at + 4)
        .and_then(|field| field.try_into().ok())
        .map(|field| u32::from_be_bytes(field) as usize)
        .ok_or_else(|| invalid("DCX header cut short"))
}

impl Dcx {
    pub fn data_mut(&mut self) -> &mut [u8] {
        &mut self.data
    }

    pub fn parse(file: &[u8]) -> io::Result<Self> {
        if file.get(..4) != Some(b"DCX\0") || file.get(0x28..0x2C) != Some(b"DFLT") {
            return Err(invalid("not a DFLT DCX"));
        }
        let size = be_u32(file, 0x1C)?;
        let mut data = Vec::with_capacity(size);
        ZlibDecoder::new(&file[HEADER..]).read_to_end(&mut data)?;
        if data.len() != size {
            return Err(invalid("DCX size does not match its contents"));
        }
        Ok(Self {
            header: file[..HEADER].to_vec(),
            data,
        })
    }

    /// The same header with both sizes updated. Level 6: the file is
    /// rebuilt at launch, and the best level costs seconds for a few
    /// percent the game never notices.
    pub fn write(&self) -> io::Result<Vec<u8>> {
        let mut encoder = ZlibEncoder::new(Vec::with_capacity(self.data.len() / 3), Compression::new(6));
        encoder.write_all(&self.data)?;
        let packed = encoder.finish()?;
        let size = |len: usize| u32::try_from(len).map_err(|_| invalid("DCX contents over 4 GiB"));
        let mut out = Vec::with_capacity(HEADER + packed.len());
        out.extend_from_slice(&self.header);
        out[0x1C..0x20].copy_from_slice(&size(self.data.len())?.to_be_bytes());
        out[0x20..0x24].copy_from_slice(&size(packed.len())?.to_be_bytes());
        out.extend_from_slice(&packed);
        Ok(out)
    }
}
