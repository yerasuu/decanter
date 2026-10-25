//! Free text as an allow-list of token shapes.
//!
//! A message is cut into tokens; a token stays only when its shape is one failure lines are made
//! of (words, small numbers, error codes, offsets, source files, image names), and anything else
//! becomes a placeholder. The normalizer and the validator classify with the same function, so
//! what the client keeps is what the server takes.

mod shapes;

use serde::Serialize;

use shapes::is_path_like;
pub use shapes::{classify, is_image, is_word};

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

/// A message may hold no proper name; a label (an OS release, a GPU or a driver) is one and stays
/// ASCII; a game's name is one and may be Unicode.
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

/// `Dell`, `Inc`, `Wireless`: a capital, then lowercase letters only.
fn is_name_word(token: &str) -> bool {
    let token = core(token);
    let mut bytes = token.bytes();
    token.len() >= 2
        && bytes.next().is_some_and(|b| b.is_ascii_uppercase())
        && bytes.all(|b| b.is_ascii_lowercase())
        && !TECHNICAL_WORDS.contains(&token)
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
fn name_runs(pieces: &[Piece<'_>]) -> Vec<bool> {
    let mut marked = vec![false; pieces.len()];
    let mut previous = None;
    let mut extending = false;
    for (index, piece) in pieces.iter().enumerate() {
        match piece {
            Piece::Token(token) if is_name_word(token) => {
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

/// Every reason `text` cannot be published as it is, each once, in order of appearance.
pub fn check(text: &str, mode: Mode) -> Vec<Private> {
    let pieces = pieces(text);
    let runs = if mode == Mode::Message {
        name_runs(&pieces)
    } else {
        vec![false; pieces.len()]
    };
    let mut found = Vec::new();
    if mode != Mode::Name && !text.is_ascii() {
        found.push(Private::NonAscii);
    }
    for (piece, in_run) in pieces.iter().zip(runs) {
        let verdict = match piece {
            Piece::Token(_) if in_run => Err(Private::ProperName),
            Piece::Token(token) => classify(token, mode),
            Piece::Sep(sep) if sep.chars().any(|c| c.is_control() && c != ' ') => Err(Private::Unrecognized),
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

/// The message as a report may carry it.
///
/// Every token [`check`] would refuse is replaced by its placeholder, and the text is cut at
/// `max` bytes between tokens. An image path keeps its file name (`libpsport-gpu.dll+0x308914`),
/// since the image tells where a fault was and the folders around it are the private part.
#[must_use]
pub fn normalize_message(text: &str, max: usize) -> String {
    let flat: String = text
        .trim()
        .chars()
        .map(|c| if c.is_whitespace() { ' ' } else { c })
        .collect();
    let pieces = pieces(&flat);
    let runs = name_runs(&pieces);
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
            Piece::Token(token) => replace(token),
        };
        if out.len() + kept.len() > max {
            break;
        }
        out.push_str(&kept);
        index += 1;
    }
    out.trim_end().to_owned()
}

fn replace(token: &str) -> String {
    let trimmed = core(token);
    let trailing = token.get(trimmed.len()..).unwrap_or_default();
    match classify(trimmed, Mode::Message) {
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
