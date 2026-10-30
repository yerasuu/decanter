//! The machine's own names, kept out of every report (decision #1034).
//!
//! A shape rule cannot tell a one-word user or host name from a technical word, but the client
//! knows both. The caller hands this crate its identity as data (the crate does no I/O) and the
//! builders ([`crate::psport_report`], [`crate::decanter_report`]) refuse any text that holds one
//! of those strings. Only the builders apply it: the server has no such list, so
//! [`crate::validate`] never does.
//!
//! A string matches case-insensitively, without accents, with `_ - .` and spaces ignored on both
//! sides (so `john_smith` finds `John Smith`), as a substring of any longer token, and across the
//! two kinds of path separator. A string shorter than three characters matches only a token that
//! is exactly it, since a substring of two letters would hit most text.

use std::borrow::Cow;
use std::ops::Range;

use crate::schema::{Build, CallCode, Calls, DecanterReport, Fault, Message, PsportReport, Report, Slot, System};
use crate::validate::{Rejection, Rule};

/// The strings that identify the person or the machine, as the client collected them.
#[derive(Clone, Debug, Default)]
pub struct Identity {
    pub user: String,
    pub host: String,
    pub home_dirs: Vec<String>,
    pub account_names: Vec<String>,
    pub display_names: Vec<String>,
    pub pad_names: Vec<String>,
    pub extra: Vec<String>,
}

impl Identity {
    /// An identity from plain strings; every name of `names` (accounts, displays, pads, anything
    /// else) is treated alike, and empty strings are ignored.
    #[must_use]
    pub fn from_strings(user: &str, host: &str, home_dirs: &[&str], names: &[&str]) -> Self {
        let own = |list: &[&str]| list.iter().map(|&s| s.to_owned()).collect();
        Self {
            user: user.to_owned(),
            host: host.to_owned(),
            home_dirs: own(home_dirs),
            extra: own(names),
            ..Self::default()
        }
    }
}

/// Path components every home path shares; they say nothing about whose it is.
const GENERIC_FOLDERS: &[&str] = &[
    "home", "users", "user", "root", "mnt", "media", "run", "var", "usr", "opt", "data",
];

/// The folded identity strings, built once and matched against every text a report or a log
/// that leaves the machine holds.
pub struct Guard {
    needles: Vec<Vec<char>>,
}

const fn is_ignorable(c: char) -> bool {
    c.is_whitespace()
        || matches!(
            c,
            '_' | '-' | '.' | '\u{ad}' | '\u{200b}'..='\u{200f}' | '\u{2060}' | '\u{feff}' | '\u{300}'..='\u{36f}'
        )
}

const fn is_boundary(c: char) -> bool {
    c.is_whitespace()
        || matches!(
            c,
            '/' | '\\' | '(' | ')' | '[' | ']' | '{' | '}' | '<' | '>' | ',' | ';' | '=' | '"' | '\'' | ':' | '|'
        )
}

/// Latin letters with diacritics to their base letter, full-width ASCII to ASCII, `\` to `/`.
fn unaccent(c: char) -> char {
    match c {
        '\\' => '/',
        '\u{ff01}'..='\u{ff5e}' => char::from_u32(u32::from(c) - 0xfee0).unwrap_or(c),
        'à'..='å' | 'ā' | 'ă' | 'ą' => 'a',
        'ç' | 'ć' | 'ĉ' | 'ċ' | 'č' => 'c',
        'ď' | 'đ' => 'd',
        'è'..='ë' | 'ē' | 'ĕ' | 'ė' | 'ę' | 'ě' => 'e',
        'ĝ' | 'ğ' | 'ġ' | 'ģ' => 'g',
        'ĥ' | 'ħ' => 'h',
        'ì'..='ï' | 'ĩ' | 'ī' | 'ĭ' | 'į' | 'ı' => 'i',
        'ĵ' => 'j',
        'ķ' => 'k',
        'ĺ' | 'ļ' | 'ľ' | 'ŀ' | 'ł' => 'l',
        'ñ' | 'ń' | 'ņ' | 'ň' => 'n',
        'ò'..='ö' | 'ø' | 'ō' | 'ŏ' | 'ő' => 'o',
        'ŕ' | 'ŗ' | 'ř' => 'r',
        'ś' | 'ŝ' | 'ş' | 'š' | 'ß' => 's',
        'ţ' | 'ť' | 'ŧ' => 't',
        'ù'..='ü' | 'ũ' | 'ū' | 'ŭ' | 'ů' | 'ű' | 'ų' => 'u',
        'ŵ' => 'w',
        'ý' | 'ÿ' | 'ŷ' => 'y',
        'ź' | 'ż' | 'ž' => 'z',
        other => other,
    }
}

/// Each kept character folded, with the byte offset of the original it came from.
fn folded(text: &str) -> Vec<(char, usize)> {
    let mut out = Vec::with_capacity(text.len());
    for (at, c) in text.char_indices().filter(|&(_, c)| !is_ignorable(c)) {
        out.extend(c.to_lowercase().map(|lower| (unaccent(lower), at)));
    }
    out
}

fn fold_chars(text: &str) -> Vec<char> {
    folded(text).into_iter().map(|(c, _)| c).collect()
}

fn char_end(text: &str, at: usize) -> usize {
    at + text
        .get(at..)
        .and_then(|rest| rest.chars().next())
        .map_or(1, char::len_utf8)
}

impl Guard {
    #[must_use]
    pub fn new(identity: &Identity) -> Self {
        let mut strings: Vec<&str> = [identity.user.as_str(), identity.host.as_str()].into();
        for list in [
            &identity.home_dirs,
            &identity.account_names,
            &identity.display_names,
            &identity.pad_names,
            &identity.extra,
        ] {
            strings.extend(list.iter().map(String::as_str));
        }
        let folders = identity.home_dirs.iter().flat_map(|dir| {
            dir.split(['/', '\\']).filter(|part| {
                let generic = GENERIC_FOLDERS.iter().any(|g| g.eq_ignore_ascii_case(part));
                let drive = part.len() == 2 && part.ends_with(':');
                !generic && !drive
            })
        });
        let mut needles: Vec<Vec<char>> = strings
            .into_iter()
            .chain(folders)
            .map(fold_chars)
            .filter(|needle| !needle.is_empty())
            .collect();
        needles.sort();
        needles.dedup();
        Self { needles }
    }

    /// The byte ranges of `text` that hold an identity string.
    fn ranges(&self, text: &str) -> Vec<Range<usize>> {
        let hay = folded(text);
        let mut out = Vec::new();
        for needle in self.needles.iter().filter(|needle| needle.len() >= 3) {
            for start in 0..hay.len() {
                let Some(window) = hay.get(start..start + needle.len()) else {
                    break;
                };
                if window.iter().map(|&(c, _)| c).eq(needle.iter().copied()) {
                    let last = window.last().map_or(0, |&(_, at)| at);
                    out.push(window.first().map_or(0, |&(_, at)| at)..char_end(text, last));
                }
            }
        }
        let short: Vec<&Vec<char>> = self.needles.iter().filter(|needle| needle.len() < 3).collect();
        if !short.is_empty() {
            let mut start = 0;
            for (at, c) in text.char_indices().chain([(text.len(), ' ')]) {
                if is_boundary(c) {
                    let token = text.get(start..at).unwrap_or_default();
                    if short.iter().any(|needle| fold_chars(token) == **needle) {
                        out.push(start..at);
                    }
                    start = at + c.len_utf8();
                }
            }
        }
        out
    }

    #[must_use]
    pub fn contains(&self, text: &str) -> bool {
        !self.ranges(text).is_empty()
    }

    /// `text` with every token that holds an identity string replaced, so nothing of the string
    /// or of what surrounds it inside its token is left (`john` in `johnsmith` must not leave
    /// `smith`). Applied to input text before it is matched against a template.
    #[must_use]
    pub fn scrub<'a>(&self, text: &'a str) -> Cow<'a, str> {
        self.scrub_counted(text).0
    }

    /// [`Self::scrub`] and how many tokens it replaced.
    #[must_use]
    pub fn scrub_counted<'a>(&self, text: &'a str) -> (Cow<'a, str>, usize) {
        let mut hits = self.ranges(text);
        if hits.is_empty() {
            return (Cow::Borrowed(text), 0);
        }
        for hit in &mut hits {
            while let Some(c) = text.get(..hit.start).and_then(|head| head.chars().next_back())
                && !is_boundary(c)
            {
                hit.start -= c.len_utf8();
            }
            while let Some(c) = text.get(hit.end..).and_then(|tail| tail.chars().next())
                && !is_boundary(c)
            {
                hit.end += c.len_utf8();
            }
        }
        hits.retain(|hit| !matches!(text.get(hit.start..hit.end), Some("redacted" | "address")));
        if hits.is_empty() {
            return (Cow::Borrowed(text), 0);
        }
        hits.sort_by_key(|hit| hit.start);
        let mut out = String::with_capacity(text.len());
        let mut at = 0;
        let mut count = 0;
        for hit in hits {
            if hit.start < at {
                at = at.max(hit.end);
                continue;
            }
            out.push_str(text.get(at..hit.start).unwrap_or_default());
            out.push_str("<redacted>");
            at = hit.end;
            count += 1;
        }
        out.push_str(text.get(at..).unwrap_or_default());
        (Cow::Owned(out), count)
    }

    fn opt(&self, value: &mut Option<String>) {
        if value.as_deref().is_some_and(|v| self.contains(v)) {
            *value = None;
        }
    }

    fn build(&self, build: &mut Build) {
        self.opt(&mut build.version);
        self.opt(&mut build.commit);
    }

    fn system(&self, system: &mut System) {
        self.opt(&mut system.os_version);
        if system.gpu.as_ref().is_some_and(|gpu| self.contains(&gpu.model)) {
            system.gpu = None;
        }
        if let Some(gpu) = &mut system.gpu {
            self.opt(&mut gpu.driver);
        }
    }

    fn slot_hits(&self, slots: &[Slot]) -> bool {
        slots.iter().any(|slot| {
            let text = match slot {
                Slot::Number(n) | Slot::Offset(n) => Cow::Owned(n.to_string()),
                Slot::Code(code) => Cow::Owned(code.to_string()),
                Slot::Call(s)
                | Slot::Version(s)
                | Slot::Image(s)
                | Slot::Vuid(s)
                | Slot::Const(s)
                | Slot::Class(s)
                | Slot::Choice(s) => Cow::Borrowed(s.as_str()),
            };
            self.contains(&text)
        })
    }

    fn message(&self, message: &mut Option<Message>) {
        let hit = message
            .as_ref()
            .is_some_and(|m| self.slot_hits(&m.slots) || m.cause.as_ref().is_some_and(|c| self.slot_hits(&c.slots)));
        if hit {
            *message = None;
        }
    }

    fn calls(&self, calls: &mut Calls) {
        for list in [
            &mut calls.unimplemented,
            &mut calls.stubs,
            &mut calls.failed,
            &mut calls.status,
        ] {
            list.retain(|call| {
                let code = call.code.map(|code| match code {
                    CallCode::Sce(n) => n.to_string(),
                    CallCode::Errno(n) => n.to_string(),
                });
                !self.contains(&call.name)
                    && !self.contains(&call.count.to_string())
                    && !code.is_some_and(|code| self.contains(&code))
            });
        }
    }

    fn fault(&self, fault: &mut Fault) {
        fault.frames.retain(|frame| {
            !frame.module.as_deref().is_some_and(|m| self.contains(m)) && !self.contains(&frame.offset.to_string())
        });
        for frame in &mut fault.frames {
            self.opt(&mut frame.symbol);
        }
    }

    fn required(&self, path: &str, value: &str, out: &mut Vec<Rejection>) {
        if self.contains(value) {
            out.push(Rejection::new(path, Rule::Identity));
        }
    }

    fn decanter(&self, report: &mut DecanterReport, out: &mut Vec<Rejection>) {
        self.required("date", &report.date, out);
        self.build(&mut report.decanter);
        self.system(&mut report.system);
        self.message(&mut report.failure.message);
    }

    fn psport(&self, report: &mut PsportReport, out: &mut Vec<Rejection>) {
        self.required("date", &report.date, out);
        self.required("game.title_id", &report.game.title_id, out);
        self.build(&mut report.decanter);
        self.build(&mut report.psport);
        self.system(&mut report.system);
        self.opt(&mut report.game.name);
        self.opt(&mut report.game.version);
        report.settings.retain(|_, value| !self.contains(value));
        self.message(&mut report.failure.message);
        if let Some(calls) = &mut report.calls {
            self.calls(calls);
        }
        if let Some(fault) = &mut report.fault {
            self.fault(fault);
        }
    }

    /// Clears every optional field that holds an identity string and rejects the report when a
    /// required one does. Returns the rejections; empty means the report is clean.
    pub fn purge(&self, report: &mut Report) -> Vec<Rejection> {
        let mut out = Vec::new();
        match report {
            Report::Decanter(report) => self.decanter(report, &mut out),
            Report::Psport(report) => self.psport(report, &mut out),
        }
        out
    }
}
