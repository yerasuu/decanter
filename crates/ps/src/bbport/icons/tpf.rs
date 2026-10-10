//! The texture table of a PS4 TPF: where each named texture's bytes are.

use std::io;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Texture {
    pub name: String,
    pub offset: usize,
    pub size: usize,
    pub width: usize,
    pub height: usize,
    /// DXGI format: 98 is BC7, 71 is BC1.
    pub dxgi: u32,
}

fn invalid(what: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, what.to_owned())
}

fn le<const N: usize>(bytes: &[u8], at: usize) -> io::Result<[u8; N]> {
    bytes
        .get(at..at + N)
        .and_then(|field| field.try_into().ok())
        .ok_or_else(|| invalid("TPF cut short"))
}

/// Entries are 36 bytes on PS4: offset, size, format, type, mips, flags,
/// width, height, texture count, unknown, name offset, flags, DXGI format.
/// Names are UTF-16 when the header says so, as Bloodborne's are.
pub fn textures(tpf: &[u8]) -> io::Result<Vec<Texture>> {
    if tpf.get(..4) != Some(b"TPF\0") {
        return Err(invalid("not a TPF"));
    }
    if tpf.get(12) != Some(&4) {
        return Err(invalid("not a PS4 TPF"));
    }
    let count = u32::from_le_bytes(le(tpf, 8)?) as usize;
    let utf16 = tpf.get(14) == Some(&1);
    let mut textures = Vec::with_capacity(count);
    for index in 0..count {
        let at = 16 + 36 * index;
        let name_at = u32::from_le_bytes(le(tpf, at + 24)?) as usize;
        let name = if utf16 {
            let units: Vec<u16> = tpf
                .get(name_at..)
                .ok_or_else(|| invalid("TPF name out of range"))?
                .chunks_exact(2)
                .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
                .take_while(|&unit| unit != 0)
                .collect();
            String::from_utf16_lossy(&units)
        } else {
            let bytes = tpf.get(name_at..).ok_or_else(|| invalid("TPF name out of range"))?;
            String::from_utf8_lossy(bytes.split(|&byte| byte == 0).next().unwrap_or_default()).into_owned()
        };
        let texture = Texture {
            name,
            offset: u32::from_le_bytes(le(tpf, at)?) as usize,
            size: u32::from_le_bytes(le(tpf, at + 4)?) as usize,
            width: u16::from_le_bytes(le(tpf, at + 12)?) as usize,
            height: u16::from_le_bytes(le(tpf, at + 14)?) as usize,
            dxgi: u32::from_le_bytes(le(tpf, at + 32)?),
        };
        if tpf.len() < texture.offset + texture.size {
            return Err(invalid("TPF texture out of range"));
        }
        textures.push(texture);
    }
    Ok(textures)
}
