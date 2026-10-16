//! Valve's *binary* `KeyValues` format — what `shortcuts.vdf` is written
//! in, as opposed to the text format `crate::vdf` parses for `.acf` files
//! and `libraryfolders.vdf`. Steam never mixes the two: a `shortcuts.vdf`
//! is binary from its first byte.
//!
//! Layout: `0x00` opens a nested object (a NUL-terminated key follows,
//! then its children, closed by a matching `0x08`), `0x01` is a string
//! leaf (NUL-terminated key, then NUL-terminated value), `0x02` is an
//! `i32` leaf (NUL-terminated key, then 4 bytes little-endian), `0x08`
//! closes the innermost still-open object.
//!
//! Unlike `crate::vdf::Value` (a `BTreeMap`, sorted by key), objects here
//! keep insertion order: `shortcuts.vdf` keys its entries "0", "1", "2", …
//! and Steam reads them in that order — a `BTreeMap` would put "10" before
//! "2" alphabetically and silently reorder anything past the 10th entry.

#![expect(
    clippy::std_instead_of_alloc,
    reason = "desktop app, no no_std target exists to justify alloc:: here"
)]
#![expect(
    clippy::std_instead_of_core,
    reason = "desktop app, no no_std target exists to justify core:: here"
)]
#![expect(
    clippy::implicit_return,
    reason = "directly contradicts needless_return from clippy::all, also enabled"
)]
#![expect(
    clippy::large_stack_frames,
    reason = "several functions here cost more than the 512-byte threshold by design, same as crate::vdf"
)]

use std::fmt;

/// One node: a leaf value, or an ordered object.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Value {
    Map(Vec<(String, Self)>),
    Str(String),
    Int(i32),
}

impl Value {
    #[must_use]
    #[inline]
    pub fn map<K: Into<String>>(entries: impl IntoIterator<Item = (K, Self)>) -> Self {
        Self::Map(entries.into_iter().map(|(key, value)| (key.into(), value)).collect())
    }

    #[must_use]
    #[inline]
    pub fn as_map(&self) -> Option<&[(String, Self)]> {
        match self {
            Self::Map(entries) => Some(entries),
            Self::Str(_) | Self::Int(_) => None,
        }
    }

    #[must_use]
    #[inline]
    pub fn get(&self, key: &str) -> Option<&Self> {
        self.as_map()?.iter().find(|(k, _)| k == key).map(|(_, v)| v)
    }

    #[must_use]
    #[inline]
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Self::Str(value) => Some(value),
            Self::Map(_) | Self::Int(_) => None,
        }
    }

    #[must_use]
    #[inline]
    pub const fn as_int(&self) -> Option<i32> {
        match self {
            Self::Int(value) => Some(*value),
            Self::Map(_) | Self::Str(_) => None,
        }
    }
}

impl From<&str> for Value {
    #[inline]
    fn from(value: &str) -> Self {
        Self::Str(value.to_owned())
    }
}

impl From<String> for Value {
    #[inline]
    fn from(value: String) -> Self {
        Self::Str(value)
    }
}

impl From<i32> for Value {
    #[inline]
    fn from(value: i32) -> Self {
        Self::Int(value)
    }
}

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    #[error("unexpected end of input")]
    UnexpectedEnd,
    #[error("unknown node type byte {0:#04x}")]
    UnknownType(u8),
    #[error("string value is not valid UTF-8")]
    InvalidUtf8,
}

impl fmt::Display for Value {
    /// Not used for the actual Steam file (see [`encode`]) — only so
    /// `Value` has a human-readable form for debugging/tests.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}

const TYPE_MAP: u8 = 0x00;
const TYPE_STR: u8 = 0x01;
const TYPE_INT: u8 = 0x02;
const TYPE_END: u8 = 0x08;

/// Encodes `root` under `root_key` as Steam writes `shortcuts.vdf`.
///
/// The file has one more closing byte than it looks like it should: `root`
/// is written as if it were the single entry of an implicit, keyless
/// level-0 object — `root_key`'s own open/children/close, *then* one more
/// [`TYPE_END`] closing that outer, never-opened object. Confirmed against
/// the reference Python `vdf` library's `_binary_dump_gen`, whose trailing
/// `yield BIN_END` runs once per call regardless of nesting depth — a
/// version of this file missing that last byte is exactly what a real
/// Steam client logs `CSteamDoc::LoadShortcuts: failed to load shortcut
/// file` against.
#[must_use]
#[inline]
pub fn encode(root_key: &str, root: &Value) -> Vec<u8> {
    let mut out = Vec::new();
    write_value(&mut out, root_key, root);
    out.push(TYPE_END);
    out
}

fn write_value(out: &mut Vec<u8>, key: &str, value: &Value) {
    match value {
        Value::Map(entries) => {
            out.push(TYPE_MAP);
            write_cstr(out, key);
            for (child_key, child) in entries {
                write_value(out, child_key, child);
            }
            out.push(TYPE_END);
        }
        Value::Str(string) => {
            out.push(TYPE_STR);
            write_cstr(out, key);
            write_cstr(out, string);
        }
        Value::Int(int) => {
            out.push(TYPE_INT);
            write_cstr(out, key);
            out.extend_from_slice(&int.to_le_bytes());
        }
    }
}

fn write_cstr(out: &mut Vec<u8>, value: &str) {
    out.extend_from_slice(value.as_bytes());
    out.push(0);
}

/// Decodes a whole `shortcuts.vdf` buffer into its root key and value.
///
/// # Errors
/// Returns an error if `bytes` ends mid-node, carries an unrecognized type
/// byte, or a string isn't valid UTF-8.
#[inline]
pub fn decode(bytes: &[u8]) -> Result<(String, Value), Error> {
    let mut cursor = Cursor { bytes, pos: 0 };
    let type_byte = cursor.read_u8()?;
    if type_byte != TYPE_MAP {
        return Err(Error::UnknownType(type_byte));
    }
    let key = cursor.read_cstr()?;
    let value = read_value(&mut cursor)?;
    // The implicit level-0 object's own closing byte — see `encode`'s doc
    // for why this isn't just `value`'s own closing `TYPE_END`.
    let closing = cursor.read_u8()?;
    if closing != TYPE_END {
        return Err(Error::UnknownType(closing));
    }
    Ok((key, value))
}

struct Cursor<'bytes> {
    bytes: &'bytes [u8],
    pos: usize,
}

impl Cursor<'_> {
    fn read_u8(&mut self) -> Result<u8, Error> {
        let byte = *self.bytes.get(self.pos).ok_or(Error::UnexpectedEnd)?;
        self.pos += 1;
        Ok(byte)
    }

    fn read_cstr(&mut self) -> Result<String, Error> {
        let start = self.pos;
        let end = self.bytes[start..]
            .iter()
            .position(|&byte| byte == 0)
            .map(|offset| start + offset)
            .ok_or(Error::UnexpectedEnd)?;
        let bytes = &self.bytes[start..end];
        self.pos = end + 1;
        String::from_utf8(bytes.to_vec()).map_err(|_utf8_error| Error::InvalidUtf8)
    }

    fn read_i32(&mut self) -> Result<i32, Error> {
        let end = self.pos.checked_add(4).ok_or(Error::UnexpectedEnd)?;
        let slice = self.bytes.get(self.pos..end).ok_or(Error::UnexpectedEnd)?;
        self.pos = end;
        Ok(i32::from_le_bytes([slice[0], slice[1], slice[2], slice[3]]))
    }
}

/// Reads the object body that follows a `0x00` type byte and its key —
/// i.e. every child entry up to the matching `0x08`.
fn read_value(cursor: &mut Cursor<'_>) -> Result<Value, Error> {
    let mut entries = Vec::new();
    loop {
        let type_byte = cursor.read_u8()?;
        if type_byte == TYPE_END {
            return Ok(Value::Map(entries));
        }
        let key = cursor.read_cstr()?;
        let value = match type_byte {
            TYPE_MAP => read_value(cursor)?,
            TYPE_STR => Value::Str(cursor.read_cstr()?),
            TYPE_INT => Value::Int(cursor.read_i32()?),
            other => return Err(Error::UnknownType(other)),
        };
        entries.push((key, value));
    }
}
