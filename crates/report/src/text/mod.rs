//! Free text as an allow-list of token shapes.
//!
//! A message is cut into tokens; a token stays only when its shape is one failure lines are made
//! of (words, small numbers, error codes, offsets, source files, image names), and anything else
//! becomes a placeholder. The normalizer and the validator classify with the same function, so
//! what the client keeps is what the server takes.

mod shapes;

use serde::Serialize;

pub use shapes::{
    classify, has_secret_prefix, is_driver, is_gpu_model_tail, is_image, is_os_version, is_pre_release, is_word,
};
use shapes::{classify_in, has_markup, is_label_word, is_path_like, strip_markup};

/// Why a token cannot stay; serialized as the rule a rejection names, never with the token.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Private {
    Path,
    Url,
    Email,
    HostName,
    IpAddress,
    Uuid,
    Address,
    LongNumber,
    Timestamp,
    NonAscii,
    ProperName,
    Unrecognized,
}

impl Private {
    const fn placeholder(self) -> &'static str {
        match self {
            Self::Path => "<path>",
            Self::Email | Self::Uuid | Self::LongNumber => "<id>",
            Self::Address => "<addr>",
            Self::IpAddress => "<ip>",
            Self::Url | Self::HostName => "<host>",
            Self::Timestamp => "<time>",
            Self::ProperName => "<name>",
            Self::NonAscii | Self::Unrecognized => "<text>",
        }
    }
}

/// A message may hold no proper name; a label (an OS release, a GPU or a driver) is made of
/// hardware and system words and stays ASCII; a game's name is a proper name and may be Unicode.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Message,
    Label,
    Name,
}

enum Piece<'a> {
    Sep(&'a str),
    Token(&'a str),
}

const fn is_separator(c: char) -> bool {
    c.is_whitespace()
        || matches!(
            c,
            '(' | ')' | '[' | ']' | '{' | '}' | ',' | ';' | '=' | '"' | '\'' | '<' | '>'
        )
}

/// A path is one token up to the next space even when separators sit inside it
/// (`C:\Users\<user>\AppData`), so no folder name survives as a word of its own.
fn pieces(text: &str) -> Vec<Piece<'_>> {
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(first) = rest.chars().next() {
        let chunk_end = rest.find(char::is_whitespace).unwrap_or(rest.len());
        let chunk = rest.get(..chunk_end).unwrap_or_default();
        let open = chunk.len() - chunk.trim_start_matches(['(', '[', '"', '\'']).len();
        let inner = chunk
            .get(open..)
            .unwrap_or_default()
            .trim_end_matches([')', ']', '"', '\'', ',', ';']);
        if !first.is_whitespace() && is_path_like(inner) {
            let (lead, tail) = rest.split_at(open);
            let (path, tail) = tail.split_at(inner.len());
            if !lead.is_empty() {
                out.push(Piece::Sep(lead));
            }
            out.push(Piece::Token(path));
            rest = tail;
            continue;
        }
        let separator = is_separator(first);
        let split = rest
            .char_indices()
            .find(|&(_, c)| is_separator(c) != separator)
            .map_or(rest.len(), |(at, _)| at);
        let (piece, tail) = rest.split_at(split);
        out.push(if separator {
            Piece::Sep(piece)
        } else {
            Piece::Token(piece)
        });
        rest = tail;
    }
    out
}

/// A token without the punctuation that ends a clause (`ERROR:`, `Inc.`), kept as written.
fn core(token: &str) -> &str {
    token.trim_end_matches([':', '.', '!', '?', '-', '*'])
}

/// Capitalized words that start failure lines (`Assertion Failed!`, `Device Lost`), not names.
const TECHNICAL_WORDS: &[&str] = &[
    "Access",
    "Assertion",
    "Denied",
    "Device",
    "Error",
    "Failed",
    "Failure",
    "Fatal",
    "Found",
    "Invalid",
    "Lost",
    "Memory",
    "Not",
    "Out",
    "Timeout",
    "Unknown",
    "Unsupported",
];

/// `Dell`, `Inc`, `Wireless`: a capital, then lowercase letters only. In a label the hardware and
/// system words (`Radeon Graphics`, `Arch Linux`) are not names.
fn is_name_word(token: &str, mode: Mode) -> bool {
    let token = core(token);
    let mut bytes = token.bytes();
    token.len() >= 2
        && bytes.next().is_some_and(|b| b.is_ascii_uppercase())
        && bytes.all(|b| b.is_ascii_lowercase())
        && !TECHNICAL_WORDS.contains(&token)
        && !(mode == Mode::Label && is_label_word(token))
}

/// `DELL`, `S2522HG`: the brand and model a device name goes on with.
fn is_model_word(token: &str) -> bool {
    let token = core(token);
    token.len() >= 2
        && token.bytes().any(|b| b.is_ascii_uppercase())
        && token.bytes().all(|b| b.is_ascii_uppercase() || b.is_ascii_digit())
}

fn is_space(sep: &str) -> bool {
    sep.bytes().all(|b| b == b' ')
}

/// Two name words in a row (`Dell Inc.`, `Wireless Controller`, `Black Ops`) read as the name of
/// a device, a person or a window, which a failure line never needs; the capitals and model
/// that follow such a run (`DELL S2522HG`) belong to it. A label ending in `:` ends a run, so
/// `Window: Call` is not one.
fn name_runs(pieces: &[Piece<'_>], mode: Mode) -> Vec<bool> {
    let mut marked = vec![false; pieces.len()];
    let mut previous = None;
    let mut extending = false;
    for (index, piece) in pieces.iter().enumerate() {
        match piece {
            Piece::Token(token) if is_name_word(token, mode) => {
                if let Some(before) = previous {
                    marked[before] = true;
                    marked[index] = true;
                    extending = true;
                }
                previous = (!token.ends_with(':')).then_some(index);
            }
            Piece::Token(token) if extending && is_model_word(token) => {
                marked[index] = true;
                previous = None;
            }
            Piece::Sep(sep) if is_space(sep) => {}
            Piece::Token(_) | Piece::Sep(_) => {
                previous = None;
                extending = false;
            }
        }
    }
    marked
}

/// Which tokens follow the word `code` or `offset`, where eight hex digits are not an address.
fn after_code(pieces: &[Piece<'_>]) -> Vec<bool> {
    let mut previous = "";
    pieces
        .iter()
        .map(|piece| match piece {
            Piece::Token(token) => {
                let after = ["code", "offset"]
                    .iter()
                    .any(|word| core(previous).eq_ignore_ascii_case(word));
                previous = token;
                after
            }
            Piece::Sep(_) => false,
        })
        .collect()
}

/// A quote or `=` is a separator between words; at either end of the text it opens or closes
/// something the reader never sees the other half of.
fn has_loose_quote(pieces: &[Piece<'_>]) -> bool {
    let loose = |piece: Option<&Piece<'_>>| matches!(piece, Some(Piece::Sep(sep)) if sep.contains(['"', '\'', '=']));
    loose(pieces.first()) || loose(pieces.last())
}

/// Every reason `text` cannot be published as it is, each once, in order of appearance.
pub fn check(text: &str, mode: Mode) -> Vec<Private> {
    let pieces = pieces(text);
    let runs = if mode == Mode::Name {
        vec![false; pieces.len()]
    } else {
        name_runs(&pieces, mode)
    };
    let codes = after_code(&pieces);
    let mut found = Vec::new();
    if mode != Mode::Name && !text.is_ascii() {
        found.push(Private::NonAscii);
    }
    if has_markup(text) || has_loose_quote(&pieces) {
        found.push(Private::Unrecognized);
    }
    for (index, (piece, in_run)) in pieces.iter().zip(runs).enumerate() {
        let verdict = match piece {
            Piece::Token(_) if in_run => Err(Private::ProperName),
            Piece::Token(token) => classify_in(token, mode, codes[index]),
            Piece::Sep(sep)
                if sep
                    .chars()
                    .any(|c| c.is_control() || matches!(c, '\u{2028}' | '\u{2029}')) =>
            {
                Err(Private::Unrecognized)
            }
            Piece::Sep(_) => Ok(()),
        };
        if let Err(reason) = verdict
            && !found.contains(&reason)
        {
            found.push(reason);
        }
    }
    found
}

/// The start of a line, at most `max` bytes cut on a character boundary: `pieces` scans the rest
/// of the line again for every token, so a megabyte with no spaces must never reach it.
pub(crate) fn cap_line(text: &str, max: usize) -> &str {
    text.get(..text.floor_char_boundary(max)).unwrap_or(text)
}

/// `text` without the quotes and `=` at its two ends, which [`check`] refuses there.
fn trim_edges(text: &str) -> String {
    let pieces = pieces(text);
    let last = pieces.len().saturating_sub(1);
    let joined: String = pieces
        .iter()
        .enumerate()
        .map(|(index, piece)| match piece {
            Piece::Sep(sep) if index == 0 || index == last => {
                sep.chars().filter(|c| !matches!(c, '"' | '\'' | '=')).collect()
            }
            Piece::Sep(piece) | Piece::Token(piece) => (*piece).to_owned(),
        })
        .collect();
    joined.trim().to_owned()
}

/// The message as a report may carry it.
///
/// Every token [`check`] would refuse is replaced by its placeholder, and the text is cut at
/// `max` bytes between tokens. An image path keeps its file name (`libpsport-gpu.dll+0x308914`),
/// since the image tells where a fault was and the folders around it are the private part.
#[must_use]
pub fn normalize_message(text: &str, max: usize) -> String {
    let flat: String = strip_markup(cap_line(text, max.saturating_mul(4)).trim())
        .chars()
        .map(|c| if c.is_whitespace() { ' ' } else { c })
        .collect();
    let pieces = pieces(&flat);
    let runs = name_runs(&pieces, Mode::Message);
    let codes = after_code(&pieces);
    let mut out = String::with_capacity(flat.len().min(max));
    let mut index = 0;
    while let Some(piece) = pieces.get(index) {
        let kept = match piece {
            Piece::Sep(sep) => (*sep).to_owned(),
            Piece::Token(_) if runs[index] => {
                while let (Some(Piece::Sep(sep)), Some(true)) = (pieces.get(index + 1), runs.get(index + 2).copied())
                    && is_space(sep)
                {
                    index += 2;
                }
                let trailing = match pieces.get(index) {
                    Some(Piece::Token(last)) => last.get(core(last).len()..).unwrap_or_default(),
                    _ => "",
                };
                format!("{}{trailing}", Private::ProperName.placeholder())
            }
            Piece::Token(token) => replace(token, codes[index]),
        };
        if out.len() + kept.len() > max {
            break;
        }
        out.push_str(&kept);
        index += 1;
    }
    trim_edges(out.trim_end())
}

fn replace(token: &str, after_code: bool) -> String {
    let trimmed = core(token);
    let trailing = token.get(trimmed.len()..).unwrap_or_default();
    match classify_in(trimmed, Mode::Message, after_code) {
        Ok(()) => token.to_owned(),
        Err(Private::Path) => {
            let name = trimmed.rsplit(['/', '\\']).next().unwrap_or_default();
            let image = name.split_once('+').map_or(name, |(image, _)| image);
            if classify(name, Mode::Message).is_ok() && is_image(image) {
                format!("{name}{trailing}")
            } else {
                format!("{}{trailing}", Private::Path.placeholder())
            }
        }
        Err(reason) => format!("{}{trailing}", reason.placeholder()),
    }
}
