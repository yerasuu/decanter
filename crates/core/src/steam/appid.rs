//! The two ids a Steam non-Steam-game shortcut needs, both derived from
//! the same CRC32 of `exe + name`.
//!
//! Steam itself computes the legacy id this same way when a user adds a
//! shortcut through its own "Add a Non-Steam Game" dialog — matching that
//! algorithm isn't required for correctness (nothing validates it), but it
//! means decanter's generated id is indistinguishable from one Steam would
//! have picked itself.

#![expect(
    clippy::implicit_return,
    reason = "directly contradicts needless_return from clippy::all, also enabled"
)]

/// IEEE CRC32 (polynomial `0xEDB88320`), the same variant `zlib`/`gzip`/PKZIP
/// use and what Steam's own id generation is built on. Table-based rather
/// than bit-looped: this runs once per install/uninstall, but the table
/// costs nothing to build and keeps the hot loop to one XOR per byte.
fn crc32(bytes: &[u8]) -> u32 {
    const fn table() -> [u32; 256] {
        let mut table = [0_u32; 256];
        let mut byte = 0;
        while byte < 256 {
            let mut crc = byte as u32;
            let mut bit = 0;
            while bit < 8 {
                crc = if crc & 1 == 1 {
                    (crc >> 1) ^ 0xEDB8_8320
                } else {
                    crc >> 1
                };
                bit += 1;
            }
            table[byte] = crc;
            byte += 1;
        }
        table
    }
    static TABLE: [u32; 256] = table();

    let mut crc = 0xFFFF_FFFF_u32;
    for &byte in bytes {
        let index = ((crc ^ u32::from(byte)) & 0xFF) as usize;
        crc = (crc >> 8) ^ TABLE[index];
    }
    crc ^ 0xFFFF_FFFF
}

/// The `appid` field written into `shortcuts.vdf` itself.
///
/// `exe` must be the exact literal string that goes in the `Exe` VDF field
/// (quotes included) — the hash input, not just the path.
#[must_use]
#[inline]
pub fn legacy_appid(exe: &str, name: &str) -> i32 {
    let hash = crc32(format!("{exe}{name}").as_bytes()) | 0x8000_0000;
    hash as i32
}

/// The wider id Steam's Library (post-2019 UI) looks artwork up by —
/// distinct from [`legacy_appid`], which only still matters for the
/// literal `appid` field in `shortcuts.vdf`. Grid image filenames use
/// this one.
#[must_use]
#[inline]
pub fn grid_appid(legacy: i32) -> u64 {
    (u64::from(legacy as u32) << 32) | 0x0200_0000
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc32_matches_known_vectors() {
        // Standard CRC32/IEEE test vectors.
        assert_eq!(crc32(b""), 0);
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
    }

    #[test]
    fn legacy_appid_is_stable_and_has_the_high_bit_set() {
        let a = legacy_appid("\"/path/to/game.exe\"", "Some Game");
        let b = legacy_appid("\"/path/to/game.exe\"", "Some Game");
        assert_eq!(a, b, "same input must hash the same every time");
        assert!(a < 0, "the 0x8000_0000 bit set makes this negative as an i32");
    }

    #[test]
    fn different_inputs_produce_different_ids() {
        let a = legacy_appid("\"/path/to/game.exe\"", "Game A");
        let b = legacy_appid("\"/path/to/game.exe\"", "Game B");
        assert_ne!(a, b);
    }

    #[test]
    fn grid_appid_embeds_the_legacy_id_in_the_high_32_bits() {
        let legacy = legacy_appid("\"/path/to/game.exe\"", "Some Game");
        let grid = grid_appid(legacy);
        assert_eq!(grid & 0xFFFF_FFFF, 0x0200_0000);
        assert_eq!((grid >> 32) as u32, legacy as u32);
    }
}
