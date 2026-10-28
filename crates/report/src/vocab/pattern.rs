//! The pattern language of message templates.
//!
//! A pattern is literal text with `{slot}` markers: `{number}`, `{offset}`, `{code}`, `{call}`,
//! `{version}`, `{image}`, `{vuid}`, `{const}`, `{class}`, `{a|b|c}` (one of the alternatives) and
//! `{_}` (text that is matched and discarded, never reported). Each typed slot reads only its own
//! shape, so a value that is not that shape makes the whole template miss.

use super::classes::CLASSES;
use crate::schema::Slot;

#[derive(Clone, Copy, Debug)]
pub enum Kind<'a> {
    Number,
    Offset,
    Code,
    Call,
    Version,
    Image,
    Vuid,
    Const,
    Class,
    Choice(&'a str),
    Drop,
}

#[derive(Debug)]
pub enum Segment<'a> {
    Literal(&'a str),
    Slot(Kind<'a>),
}

fn kind(name: &str) -> Option<Kind<'_>> {
    Some(match name {
        "number" => Kind::Number,
        "offset" => Kind::Offset,
        "code" => Kind::Code,
        "call" => Kind::Call,
        "version" => Kind::Version,
        "image" => Kind::Image,
        "vuid" => Kind::Vuid,
        "const" => Kind::Const,
        "class" => Kind::Class,
        "_" => Kind::Drop,
        choice if choice.contains('|') => Kind::Choice(choice),
        _ => return None,
    })
}

pub fn parse(pattern: &str) -> Option<Vec<Segment<'_>>> {
    let mut out = Vec::new();
    let mut rest = pattern;
    while !rest.is_empty() {
        if let Some(open) = rest.strip_prefix('{') {
            let (name, tail) = open.split_once('}')?;
            out.push(Segment::Slot(kind(name)?));
            rest = tail;
        } else {
            let (literal, tail) = rest.split_at(rest.find('{').unwrap_or(rest.len()));
            out.push(Segment::Literal(literal));
            rest = tail;
        }
    }
    Some(out)
}

fn run_of(text: &str, keep: impl Fn(u8) -> bool) -> usize {
    text.bytes().take_while(|&b| keep(b)).count()
}

fn hex_digits(text: &str) -> Option<(u32, usize)> {
    let len = run_of(text, |b| b.is_ascii_hexdigit());
    if !(1..=8).contains(&len) {
        return None;
    }
    Some((u32::from_str_radix(text.get(..len)?, 16).ok()?, len))
}

fn offset(text: &str) -> Option<(u32, usize)> {
    let sign = usize::from(text.starts_with('+'));
    let digits = text.get(sign..)?.strip_prefix("0x")?;
    let (value, len) = hex_digits(digits)?;
    Some((value, sign + 2 + len))
}

fn code(text: &str) -> Option<(i64, usize)> {
    if let Some((value, len)) = text.strip_prefix("0x").and_then(hex_digits) {
        return Some((i64::from(value), len + 2));
    }
    let negative = usize::from(text.starts_with('-'));
    let len = run_of(text.get(negative..)?, |b| b.is_ascii_digit());
    if !(1..=10).contains(&len) {
        return None;
    }
    let value: i64 = text.get(negative..negative + len)?.parse().ok()?;
    let value = if negative == 1 { -value } else { value };
    is_code(value).then_some((value, negative + len))
}

/// An SCE code is `0x80xxxxxx` (2.1 billion and up, or its negative); a positive value between
/// 1 and 2 billion is more likely a Unix time than a code, so it is refused.
const fn is_code(value: i64) -> bool {
    matches!(value, -999_999_999..=999_999_999 | 0x8000_0000..=0xffff_ffff | -0x8000_0000..=-2_000_000_000)
}

fn is_call_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'_' | b':' | b'~')
}

/// No run of four digits: ids, times and tokens carry them, function names (`Ngs2`) do not.
pub fn is_call(text: &str) -> bool {
    text.len() <= 128
        && !text.as_bytes().windows(4).any(|w| w.iter().all(u8::is_ascii_digit))
        && text
            .bytes()
            .next()
            .is_some_and(|b| b.is_ascii_alphabetic() || matches!(b, b'_' | b'~'))
        && text.bytes().all(is_call_byte)
}

fn call(text: &str) -> Option<(&str, usize)> {
    let name = text.get(..run_of(text, is_call_byte))?.trim_end_matches(':');
    let len = name.len();
    let parens = if text.get(len..)?.starts_with("()") { 2 } else { 0 };
    is_call(name).then_some((name, len + parens))
}

pub fn is_version(text: &str) -> bool {
    let groups: Vec<&str> = text.split('.').collect();
    (2..=4).contains(&groups.len())
        && groups
            .iter()
            .all(|g| (1..=5).contains(&g.len()) && g.bytes().all(|b| b.is_ascii_digit()))
}

fn version(text: &str) -> Option<(&str, usize)> {
    let mut end = 0;
    let mut groups = 0;
    loop {
        let digits = run_of(text.get(end..)?, |b| b.is_ascii_digit());
        if !(1..=5).contains(&digits) {
            break;
        }
        end += digits;
        groups += 1;
        let more = text
            .get(end..)?
            .strip_prefix('.')
            .is_some_and(|t| t.starts_with(|c: char| c.is_ascii_digit()));
        if !more || groups == 4 {
            break;
        }
        end += 1;
    }
    (groups >= 2).then(|| (text.get(..end).unwrap_or_default(), end))
}

pub fn is_vuid(text: &str) -> bool {
    text.len() <= 96
        && text
            .strip_prefix("VUID-")
            .is_some_and(|tail| !tail.is_empty() && tail.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-'))
}

pub fn is_const(text: &str) -> bool {
    (3..=48).contains(&text.len())
        && text.bytes().next().is_some_and(|b| b.is_ascii_uppercase())
        && text
            .bytes()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
}

pub fn is_image(text: &str) -> bool {
    const EXTENSIONS: &[&str] = &["dll", "so", "prx", "sprx", "exe", "elf", "bin", "self", "dylib"];
    text.len() <= 64
        && text.rsplit_once('.').is_some_and(|(stem, extension)| {
            EXTENSIONS.contains(&extension)
                && !stem.is_empty()
                && stem
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.'))
        })
}

fn choice<'a>(spec: &'a str, text: &str) -> Option<&'a str> {
    spec.split('|')
        .filter(|alternative| !alternative.is_empty() && text.starts_with(alternative))
        .max_by_key(|alternative| alternative.len())
}

pub fn is_choice(spec: &str, value: &str) -> bool {
    spec.split('|').any(|alternative| alternative == value)
}

fn scan(kind: Kind<'_>, text: &str) -> Option<(Slot, usize)> {
    match kind {
        Kind::Number => {
            let len = run_of(text, |b| b.is_ascii_digit());
            if !(1..=9).contains(&len) {
                return None;
            }
            Some((Slot::Number(text.get(..len)?.parse().ok()?), len))
        }
        Kind::Offset => offset(text).map(|(value, len)| (Slot::Offset(value), len)),
        Kind::Code => code(text).map(|(value, len)| (Slot::Code(value), len)),
        Kind::Call => call(text).map(|(name, len)| (Slot::Call(name.to_owned()), len)),
        Kind::Version => version(text).map(|(value, len)| (Slot::Version(value.to_owned()), len)),
        Kind::Vuid => {
            let len = run_of(text, |b| b.is_ascii_alphanumeric() || b == b'-');
            let value = text.get(..len)?;
            is_vuid(value).then(|| (Slot::Vuid(value.to_owned()), len))
        }
        Kind::Const => {
            let len = run_of(text, |b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_');
            let value = text.get(..len)?;
            is_const(value).then(|| (Slot::Const(value.to_owned()), len))
        }
        Kind::Class => {
            let len = run_of(text, |b| b.is_ascii_alphanumeric() || b == b'.');
            let value = text.get(..len)?;
            CLASSES.contains(&value).then(|| (Slot::Class(value.to_owned()), len))
        }
        Kind::Choice(spec) => choice(spec, text).map(|value| (Slot::Choice(value.to_owned()), value.len())),
        Kind::Image | Kind::Drop => None,
    }
}

/// The image's file name out of a path: folders are the private part and are dropped here.
fn image_name(text: &str) -> Option<&str> {
    let name = text.rsplit(['/', '\\']).next()?;
    is_image(name).then_some(name)
}

/// Matches `input` from its start and returns the reported slots with what the pattern left over.
pub fn run<'t>(segments: &[Segment<'_>], input: &'t str) -> Option<(Vec<Slot>, &'t str)> {
    let mut rest = input;
    let mut slots = Vec::new();
    for (index, segment) in segments.iter().enumerate() {
        match segment {
            Segment::Literal(literal) => rest = rest.strip_prefix(literal)?,
            Segment::Slot(kind @ (Kind::Drop | Kind::Image)) => {
                let end = match segments.get(index + 1) {
                    Some(Segment::Literal(next)) => rest.find(next)?,
                    Some(Segment::Slot(_)) => return None,
                    None => rest.len(),
                };
                let (taken, tail) = rest.split_at(end);
                if matches!(kind, Kind::Image) {
                    slots.push(Slot::Image(image_name(taken)?.to_owned()));
                }
                rest = tail;
            }
            Segment::Slot(kind) => {
                let (slot, len) = scan(*kind, rest)?;
                slots.push(slot);
                rest = rest.get(len..)?;
            }
        }
    }
    Some((slots, rest))
}

/// Whether a received value is something its slot kind could have read.
pub fn fits(kind: Kind<'_>, slot: &Slot) -> bool {
    match (kind, slot) {
        (Kind::Number, Slot::Number(_)) | (Kind::Offset, Slot::Offset(_)) => true,
        (Kind::Code, Slot::Code(value)) => is_code(*value),
        (Kind::Call, Slot::Call(value)) => is_call(value),
        (Kind::Version, Slot::Version(value)) => is_version(value),
        (Kind::Image, Slot::Image(value)) => is_image(value),
        (Kind::Vuid, Slot::Vuid(value)) => is_vuid(value),
        (Kind::Const, Slot::Const(value)) => is_const(value),
        (Kind::Class, Slot::Class(value)) => CLASSES.contains(&value.as_str()),
        (Kind::Choice(spec), Slot::Choice(value)) => is_choice(spec, value),
        _ => false,
    }
}

/// A slot's value as the log writes it: offsets and SCE codes in hex, the rest as they are.
pub fn show(slot: &Slot) -> String {
    match slot {
        Slot::Number(value) => value.to_string(),
        Slot::Offset(value) => format!("0x{value:x}"),
        Slot::Code(value @ 0x8000_0000..=0xffff_ffff) => format!("0x{value:08x}"),
        Slot::Code(value) => value.to_string(),
        Slot::Call(text)
        | Slot::Version(text)
        | Slot::Image(text)
        | Slot::Vuid(text)
        | Slot::Const(text)
        | Slot::Class(text)
        | Slot::Choice(text) => text.clone(),
    }
}
