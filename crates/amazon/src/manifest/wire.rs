// SPDX-License-Identifier: GPL-3.0-or-later
//! The protobuf wire format, as far as the manifest's messages need it
//! ([Protocol Buffers: encoding](https://protobuf.dev/programming-guides/encoding/)): varints,
//! length-delimited fields and the fixed-width ones skipped. Every length is checked against what
//! is left of the message before anything is sliced or allocated.

use super::ManifestError;

/// A field's value. Fixed-width values are skipped by every message here, so only their presence is kept.
pub(super) enum Value<'msg> {
    Varint(u64),
    Bytes(&'msg [u8]),
    Fixed,
}

impl<'msg> Value<'msg> {
    pub(super) const fn varint(&self, field: &'static str) -> Result<u64, ManifestError> {
        match *self {
            Self::Varint(value) => Ok(value),
            Self::Bytes(_) | Self::Fixed => Err(ManifestError::Malformed(field)),
        }
    }

    pub(super) const fn bytes(&self, field: &'static str) -> Result<&'msg [u8], ManifestError> {
        match *self {
            Self::Bytes(bytes) => Ok(bytes),
            Self::Varint(_) | Self::Fixed => Err(ManifestError::Malformed(field)),
        }
    }

    pub(super) fn string(&self, field: &'static str) -> Result<&'msg str, ManifestError> {
        core::str::from_utf8(self.bytes(field)?).map_err(|_invalid| ManifestError::Malformed(field))
    }
}

/// The fields of one message, in wire order. A repeated singular field is not refused: the last
/// one wins, as protobuf's own parsers do.
pub(super) struct Fields<'msg> {
    rest: &'msg [u8],
}

impl<'msg> Fields<'msg> {
    pub(super) const fn new(message: &'msg [u8]) -> Self {
        Self { rest: message }
    }

    /// The next field number and its value, or `None` at the end of the message.
    pub(super) fn next_field(&mut self) -> Result<Option<(u64, Value<'msg>)>, ManifestError> {
        if self.rest.is_empty() {
            return Ok(None);
        }
        let key = self.varint()?;
        let number = key >> 3_u32;
        if number == 0 || number > MAX_FIELD_NUMBER {
            return Err(ManifestError::Malformed("field number out of range"));
        }
        let value = match key & 7 {
            0 => Value::Varint(self.varint()?),
            1 => {
                self.take(8)?;
                Value::Fixed
            }
            2 => {
                let len = usize::try_from(self.varint()?)
                    .map_err(|_overflow| ManifestError::Malformed("length-delimited field too long"))?;
                Value::Bytes(self.take(len)?)
            }
            5 => {
                self.take(4)?;
                Value::Fixed
            }
            // Groups (3 and 4) are proto2's deprecated encoding, which no message here uses.
            _ => return Err(ManifestError::Malformed("unsupported wire type")),
        };
        Ok(Some((number, value)))
    }

    fn take(&mut self, len: usize) -> Result<&'msg [u8], ManifestError> {
        let (taken, rest) = self
            .rest
            .split_at_checked(len)
            .ok_or(ManifestError::Malformed("field runs past the end of its message"))?;
        self.rest = rest;
        Ok(taken)
    }

    /// A varint of at most ten bytes, the longest a `u64` takes; a longer one is refused rather
    /// than read on.
    fn varint(&mut self) -> Result<u64, ManifestError> {
        let mut value = 0_u64;
        let mut shift = 0_u32;
        for (index, &byte) in self.rest.iter().enumerate().take(10) {
            // The tenth byte's bits past the 64th fall off, as in protobuf's own decoders.
            value |= u64::from(byte & 0x7f).checked_shl(shift).unwrap_or(0);
            if byte & 0x80 == 0 {
                self.rest = self.rest.get(index.saturating_add(1)..).unwrap_or_default();
                return Ok(value);
            }
            shift = shift.saturating_add(7);
        }
        Err(ManifestError::Malformed("truncated or over-long varint"))
    }
}

/// The largest field number protobuf allows (2^29 - 1).
const MAX_FIELD_NUMBER: u64 = (1 << 29) - 1;
