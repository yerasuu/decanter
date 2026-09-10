//! Valve's `KeyValues` text format — what `appmanifest_*.acf` and
//! `libraryfolders.vdf` are written in.
//!
//! Hand-rolled rather than taken as a dependency: the format is a nesting
//! of quoted strings, and decanter both writes and reads every file it
//! parses, so the exotic corners of real Valve VDF (`#include`, unquoted
//! keys, conditionals like `[$WIN32]`) never come up. What is here is what
//! Steam actually emits.
//!
//! Parsing is tolerant on the way in — an unquoted token is accepted, a
//! `//` comment is skipped — because these files sit in a directory users
//! poke at, and refusing to read a hand-edited manifest would read to them
//! as their game vanishing.

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
    reason = "most functions here cost more than the 512-byte threshold: a couple of String \
              locals plus a Result<Value, Error> already clears it"
)]
#![expect(
    clippy::panic,
    reason = "let_underscore_must_use demands proving the writeln! Result was looked at, and \
              String's Write impl never actually fails, so the only proof is an unreachable panic"
)]

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::str::Chars;

/// One node: either a leaf string or a nested block.
///
/// Blocks keep their keys sorted rather than in file order. Steam's own
/// order isn't meaningful, and a stable order means rewriting a manifest
/// produces a stable diff.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Value {
    Block(BTreeMap<String, Self>),
    String(String),
}

impl Value {
    #[must_use]
    #[inline]
    pub const fn as_block(&self) -> Option<&BTreeMap<String, Self>> {
        match self {
            Self::Block(block) => Some(block),
            Self::String(_) => None,
        }
    }

    #[must_use]
    #[inline]
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Self::String(value) => Some(value),
            Self::Block(_) => None,
        }
    }

    #[must_use]
    #[inline]
    pub const fn block() -> Self {
        Self::Block(BTreeMap::new())
    }

    /// Leaf lookup by key. `None` for a missing key and for one that holds
    /// a block — a caller wanting a string has nothing to do with either.
    #[must_use]
    #[inline]
    pub fn get(&self, key: &str) -> Option<&str> {
        let block = match self.as_block() {
            Some(block) => block,
            None => return None,
        };
        let value = match block.get(key) {
            Some(value) => value,
            None => return None,
        };
        value.as_str()
    }

    #[must_use]
    #[inline]
    pub fn get_block(&self, key: &str) -> Option<&Self> {
        let block = match self.as_block() {
            Some(block) => block,
            None => return None,
        };
        let value = match block.get(key) {
            Some(value) => value,
            None => return None,
        };
        matches!(value, Self::Block(_)).then_some(value)
    }

    /// Parses a number written as a VDF string, which is how Steam stores
    /// every integer. `None` rather than an error: a manifest with a
    /// mangled size is still a manifest.
    #[must_use]
    #[inline]
    pub fn get_u64(&self, key: &str) -> Option<u64> {
        let value = match self.get(key) {
            Some(value) => value,
            None => return None,
        };
        value.parse().ok()
    }

    #[inline]
    pub fn insert<K, V>(&mut self, key: K, value: V)
    where
        K: Into<String>,
        V: Into<Self>,
    {
        if let Self::Block(block) = self {
            block.insert(key.into(), value.into());
        }
    }
}

impl From<&str> for Value {
    #[inline]
    fn from(value: &str) -> Self {
        Self::String(value.to_owned())
    }
}

impl From<String> for Value {
    #[inline]
    fn from(value: String) -> Self {
        Self::String(value)
    }
}

impl From<u64> for Value {
    #[inline]
    fn from(value: u64) -> Self {
        Self::String(value.to_string())
    }
}

impl From<i64> for Value {
    #[inline]
    fn from(value: i64) -> Self {
        Self::String(value.to_string())
    }
}

// `error_impl_error` wants this enum renamed away from the generic
// `Error`; `VdfError` was tried, but that name starting with the module's
// own name trips `module_name_repetitions` right back. No spelling
// satisfies both, so this keeps `Error` and leaves `error_impl_error` as
// reported.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    #[error("expected a value after key {0:?}")]
    MissingValue(String),
    #[error("unexpected `}}`")]
    UnexpectedClose,
    #[error("unexpected end of input")]
    UnexpectedEnd,
}

/// One lexical token produced by the tokenizer.
enum Token {
    /// `}`.
    Close,
    /// `{`.
    Open,
    /// A quoted or unquoted string, used for both keys and leaf values.
    String(String),
}

/// A lazy, character-at-a-time cursor over the input being tokenized.
struct Tokenizer<'text> {
    /// Input not yet turned into tokens.
    rest: Chars<'text>,
}

impl Tokenizer<'_> {
    /// Reads the next token, skipping whitespace and `//` comments.
    fn next_token(&mut self) -> Result<Option<Token>, Error> {
        loop {
            let mut peek = self.rest.clone();
            let Some(ch) = peek.next() else {
                return Ok(None);
            };

            if ch.is_whitespace() {
                self.rest = peek;
                continue;
            }
            // `//` runs to end of line. Steam writes these in
            // `libraryfolders.vdf` from time to time. Checked against a
            // separate clone of `peek` so a lone `/` (not a comment) doesn't
            // leave `peek` advanced past the character after it.
            let mut comment_peek = peek.clone();
            if ch == '/' && comment_peek.next() == Some('/') {
                self.rest = comment_peek;
                for comment_char in self.rest.by_ref() {
                    if comment_char == '\n' {
                        break;
                    }
                }
                continue;
            }
            self.rest = peek;

            return match ch {
                '{' => Ok(Some(Token::Open)),
                '}' => Ok(Some(Token::Close)),
                '"' => Ok(Some(Token::String(self.quoted()))),
                // Unquoted token: read to the next delimiter. Not
                // something Steam emits, but hand-edited files have it.
                _ => Ok(Some(Token::String(self.unquoted(ch)))),
            };
        }
    }

    /// Reads a double-quoted string, unescaping `\\`, `\"`, `\n`, and `\t`.
    fn quoted(&mut self) -> String {
        let mut value = String::new();
        while let Some(ch) = self.rest.next() {
            match ch {
                '"' => break,
                '\\' => match self.rest.next() {
                    Some('n') => value.push('\n'),
                    Some('t') => value.push('\t'),
                    // `\\` and `\"` unescape to themselves; anything else
                    // is passed through as written rather than dropped.
                    Some(other) => value.push(other),
                    None => break,
                },
                _ => value.push(ch),
            }
        }
        value
    }

    /// Reads an unquoted token starting at `first`, up to the next
    /// delimiter.
    fn unquoted(&mut self, first: char) -> String {
        let mut value = String::from(first);
        loop {
            let mut peek = self.rest.clone();
            match peek.next() {
                Some(ch) if !ch.is_whitespace() && ch != '{' && ch != '}' && ch != '"' => {
                    value.push(ch);
                    self.rest = peek;
                }
                _ => return value,
            }
        }
    }
}

/// Renders `root` as Steam writes it: the root key, then a tab-indented
/// block. Values are separated from keys by two tabs, matching the files
/// Steam produces closely enough to diff against them.
#[must_use]
#[inline]
pub fn to_string(root_key: &str, root: &Value) -> String {
    let mut out = String::new();
    write_value(&mut out, root_key, root, 0);
    out
}

/// Writes one key/value pair at `depth`, recursing into nested blocks.
fn write_value(out: &mut String, key: &str, value: &Value, depth: usize) {
    let indent = "\t".repeat(depth);
    match value {
        Value::String(string) => {
            let Ok(()) = writeln!(out, "{indent}\"{}\"\t\t\"{}\"", escape(key), escape(string)) else {
                panic!("write! to a String cannot fail")
            };
        }
        Value::Block(block) => {
            let Ok(()) = writeln!(out, "{indent}\"{}\"", escape(key)) else {
                panic!("write! to a String cannot fail")
            };
            let Ok(()) = writeln!(out, "{indent}{{") else {
                panic!("write! to a String cannot fail")
            };
            for (child_key, child) in block {
                write_value(out, child_key, child, depth.saturating_add(1));
            }
            let Ok(()) = writeln!(out, "{indent}}}") else {
                panic!("write! to a String cannot fail")
            };
        }
    }
}

/// Backslashes and quotes are the only characters VDF escapes; a literal
/// Windows path (`_CommonRedist\vcredist\...`) is the reason it has to.
fn escape(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

/// Parses a whole file, returning its single root entry.
///
/// # Errors
/// Returns an error if `input` isn't well-formed VDF: unbalanced braces,
/// a key with no value, or nothing to parse at all.
#[inline]
pub fn from_str(input: &str) -> Result<(String, Value), Error> {
    let mut tokens = Tokenizer { rest: input.chars() };
    let token = match tokens.next_token() {
        Ok(token) => token,
        Err(err) => return Err(err),
    };
    let key = match token {
        Some(Token::String(key)) => key,
        Some(Token::Close) => return Err(Error::UnexpectedClose),
        Some(Token::Open) => return Err(Error::MissingValue(String::new())),
        None => return Err(Error::UnexpectedEnd),
    };
    let value = match parse_value(&mut tokens, &key) {
        Ok(value) => value,
        Err(err) => return Err(err),
    };
    Ok((key, value))
}

/// Parses the value that follows `key`: a leaf string, or `{ ... }`
/// recursing into a block via [`parse_block`].
fn parse_value(tokens: &mut Tokenizer<'_>, key: &str) -> Result<Value, Error> {
    let token = match tokens.next_token() {
        Ok(token) => token,
        Err(err) => return Err(err),
    };
    match token {
        Some(Token::String(value)) => Ok(Value::String(value)),
        Some(Token::Open) => parse_block(tokens),
        Some(Token::Close) => Err(Error::UnexpectedClose),
        None => Err(Error::MissingValue(key.to_owned())),
    }
}

/// Parses `{ ... }` after an `Open` token, collecting key/value pairs
/// until the matching `Close`.
fn parse_block(tokens: &mut Tokenizer<'_>) -> Result<Value, Error> {
    let mut block = BTreeMap::new();
    loop {
        let token = match tokens.next_token() {
            Ok(token) => token,
            Err(err) => return Err(err),
        };
        match token {
            Some(Token::Close) => return Ok(Value::Block(block)),
            Some(Token::String(key)) => {
                let value = match parse_value(tokens, &key) {
                    Ok(value) => value,
                    Err(err) => return Err(err),
                };
                block.insert(key, value);
            }
            Some(Token::Open) => return Err(Error::MissingValue(String::new())),
            None => return Err(Error::UnexpectedEnd),
        }
    }
}
