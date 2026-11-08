// SPDX-License-Identifier: GPL-3.0-or-later
//! Little-endian reads over a manifest body that never run past its end (spec, section 1), Unreal's
//! `FString` (spec, 6.2) and the sized sections of spec 6.3, whose unread tail is skipped so that a
//! newer section version's extra fields are tolerated.

use super::{Guid, MAX_STRING_UNITS, ManifestError};

pub(super) struct Reader<'bytes> {
    bytes: &'bytes [u8],
    pos: usize,
}

impl<'bytes> Reader<'bytes> {
    pub(super) const fn new(bytes: &'bytes [u8]) -> Self {
        Self { bytes, pos: 0 }
    }

    pub(super) const fn pos(&self) -> usize {
        self.pos
    }

    pub(super) const fn remaining(&self) -> usize {
        self.bytes.len().saturating_sub(self.pos)
    }

    pub(super) fn take(&mut self, len: usize) -> Result<&'bytes [u8], ManifestError> {
        let end = self
            .pos
            .checked_add(len)
            .ok_or(ManifestError::Malformed("a length runs past the end"))?;
        let taken = self
            .bytes
            .get(self.pos..end)
            .ok_or(ManifestError::Malformed("a field runs past the end of its section"))?;
        self.pos = end;
        Ok(taken)
    }

    pub(super) fn array<const N: usize>(&mut self) -> Result<[u8; N], ManifestError> {
        let mut out = [0_u8; N];
        out.copy_from_slice(self.take(N)?);
        Ok(out)
    }

    pub(super) fn u8(&mut self) -> Result<u8, ManifestError> {
        Ok(self.array::<1>()?[0])
    }

    pub(super) fn u32(&mut self) -> Result<u32, ManifestError> {
        Ok(u32::from_le_bytes(self.array()?))
    }

    pub(super) fn i32(&mut self) -> Result<i32, ManifestError> {
        Ok(i32::from_le_bytes(self.array()?))
    }

    pub(super) fn u64(&mut self) -> Result<u64, ManifestError> {
        Ok(u64::from_le_bytes(self.array()?))
    }

    pub(super) fn i64(&mut self) -> Result<i64, ManifestError> {
        Ok(i64::from_le_bytes(self.array()?))
    }

    /// Four little-endian `u32` words (spec, 6.4 form a).
    pub(super) fn guid(&mut self) -> Result<Guid, ManifestError> {
        Ok(Guid([self.u32()?, self.u32()?, self.u32()?, self.u32()?]))
    }

    /// A count read before that many records of at least `min_size` bytes each, refused when the
    /// section could not hold them, so a forged count never sizes an allocation.
    pub(super) fn count(&mut self, min_size: usize, limit: usize, what: &'static str) -> Result<usize, ManifestError> {
        let count = usize::try_from(self.u32()?).unwrap_or(usize::MAX);
        if count > limit {
            return Err(ManifestError::TooMany { what, limit });
        }
        if count.saturating_mul(min_size) > self.remaining() {
            return Err(ManifestError::Malformed("a count is larger than its section can hold"));
        }
        Ok(count)
    }

    /// Unreal's `FString`: a positive length counts ASCII bytes, a negative one UTF-16LE units,
    /// both with their terminating NUL; zero is the empty string.
    pub(super) fn fstring(&mut self) -> Result<String, ManifestError> {
        let len = self.i32()?;
        if len == 0 {
            return Ok(String::new());
        }
        let units = usize::try_from(len.unsigned_abs()).unwrap_or(usize::MAX);
        if units > MAX_STRING_UNITS {
            return Err(ManifestError::TooLarge {
                what: "string",
                limit: MAX_STRING_UNITS,
            });
        }
        if len > 0 {
            let (text, nul) = self
                .take(units)?
                .split_last()
                .map(|(nul, text)| (text, *nul))
                .ok_or(ManifestError::Malformed("an empty narrow string"))?;
            // The spec reads narrow strings as ASCII; what Unreal writes past 0x7F is not known, so it
            // is refused rather than guessed at.
            if nul != 0 || !text.is_ascii() {
                return Err(ManifestError::Malformed("a narrow string is not NUL-ended ASCII"));
            }
            return Ok(text.iter().copied().map(char::from).collect());
        }
        let raw = self.take(units.saturating_mul(2))?;
        let mut wide: Vec<u16> = raw
            .chunks_exact(2)
            .map(|pair| u16::from_le_bytes([pair.first().copied().unwrap_or(0), pair.get(1).copied().unwrap_or(0)]))
            .collect();
        if wide.pop() != Some(0) {
            return Err(ManifestError::Malformed("a wide string is not NUL-ended"));
        }
        String::from_utf16(&wide).map_err(|_| ManifestError::Malformed("a wide string is not valid UTF-16"))
    }

    /// The next section (spec, 6.3): its `u32` size counts itself, and this reader moves past the
    /// whole of it whatever the returned one reads.
    pub(super) fn section(&mut self, what: &'static str) -> Result<Self, ManifestError> {
        let start = self.pos;
        let size = usize::try_from(self.u32()?).unwrap_or(usize::MAX);
        let end = start.saturating_add(size);
        let bytes = self
            .bytes
            .get(start..end)
            .filter(|_| size >= 4)
            .ok_or(ManifestError::Malformed(what))?;
        self.pos = end;
        Ok(Self { bytes, pos: 4 })
    }
}
