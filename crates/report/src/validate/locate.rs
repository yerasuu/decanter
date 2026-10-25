//! The field a parse error stopped at.
//!
//! `serde_json` says where a parse failed by line and column only; walking the raw JSON up to
//! that point names the field, so a rejection can say which one without a path-tracking
//! deserializer.

use serde_json::error::Category;

use super::{Rejection, Rule};
use crate::text::is_word;

enum Level {
    Object { key: Option<String>, expects_key: bool },
    Array { index: usize },
}

pub fn rejection(bytes: &[u8], error: &serde_json::Error) -> Rejection {
    let text = error.to_string();
    let rule = match error.classify() {
        Category::Io | Category::Syntax | Category::Eof => Rule::Syntax,
        Category::Data if text.starts_with("unknown field") => Rule::UnknownField,
        Category::Data if text.starts_with("missing field") => Rule::MissingField,
        Category::Data if text.starts_with("duplicate field") => Rule::DuplicateField,
        Category::Data if text.starts_with("unknown variant") => Rule::UnknownVariant,
        Category::Data => Rule::WrongType,
    };
    let mut path = path_at(bytes, offset(bytes, error.line(), error.column()));
    if rule == Rule::MissingField
        && let Some(name) = text.split('`').nth(1)
    {
        push_key(&mut path, name);
    }
    Rejection::new(path, rule)
}

fn offset(bytes: &[u8], line: usize, column: usize) -> usize {
    let start: usize = bytes
        .split(|&b| b == b'\n')
        .take(line.saturating_sub(1))
        .map(|l| l.len() + 1)
        .sum();
    start + column.saturating_sub(1)
}

/// A key that is not one of the schema's own names may be the private part, so it is not echoed.
fn push_key(path: &mut String, key: &str) {
    if !path.is_empty() {
        path.push('.');
    }
    path.push_str(if key.len() <= 40 && is_word(key) { key } else { "<key>" });
}

fn path_at(bytes: &[u8], offset: usize) -> String {
    let mut stack: Vec<Level> = Vec::new();
    let mut at = 0;
    while at <= offset && at < bytes.len() {
        match bytes[at] {
            b'{' => stack.push(Level::Object {
                key: None,
                expects_key: true,
            }),
            b'[' => stack.push(Level::Array { index: 0 }),
            b'}' | b']' => {
                stack.pop();
            }
            b',' => match stack.last_mut() {
                Some(Level::Object { expects_key, .. }) => *expects_key = true,
                Some(Level::Array { index }) => *index += 1,
                None => {}
            },
            b':' => {
                if let Some(Level::Object { expects_key, .. }) = stack.last_mut() {
                    *expects_key = false;
                }
            }
            b'"' => {
                let end = string_end(bytes, at + 1);
                if let Some(Level::Object { key, expects_key: true }) = stack.last_mut() {
                    *key = Some(String::from_utf8_lossy(bytes.get(at + 1..end).unwrap_or_default()).into_owned());
                }
                at = end;
            }
            _ => {}
        }
        at += 1;
    }
    let mut path = String::new();
    for level in &stack {
        match level {
            Level::Object { key: Some(key), .. } => push_key(&mut path, key),
            Level::Object { key: None, .. } => {}
            Level::Array { index } => path.push_str(&format!("[{index}]")),
        }
    }
    path
}

fn string_end(bytes: &[u8], mut at: usize) -> usize {
    while let Some(&b) = bytes.get(at) {
        match b {
            b'\\' => at += 2,
            b'"' => return at,
            _ => at += 1,
        }
    }
    bytes.len()
}
