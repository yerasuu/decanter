//! Secrets out of what decanter logs about the processes it starts and the lines they print.
//!
//! One policy for the run log header, failure records and the debug log. Only what is logged goes
//! through it; the environment and arguments a child gets stay as they were. A value is replaced
//! whole by [`REDACTED`]; names stay, so a log still says what was set.

use core::net::{Ipv4Addr, Ipv6Addr};
use core::ops::Range;
use std::borrow::Cow;

pub const REDACTED: &str = "<redacted>";
pub const ADDRESS: &str = "<address>";

/// How many values [`outgoing`] replaced, so the player is told before a log leaves the machine.
#[derive(Clone, Copy, Default)]
pub struct Hidden {
    pub values: usize,
    pub ids: usize,
    pub addresses: usize,
}

/// psport's `env.txt` list (`src/runtime/runtime_debug.c`, `is_secret_name`) with `PASS` and
/// `KEY` left to [`is_secret_last_word`]: as substrings they also hid `PS_KEY_*`,
/// `PS_MENU_KEYS_FILE` and `PS_PASS_BREAK_TRACE`, which a run log is read for. The shadNet
/// password and token (`PS_SHADNET_*`, `BB_SHADNET_*`) fall under `PASSWORD` and `TOKEN`.
const SECRET_WORDS: [&[u8]; 11] = [
    b"PASSWORD",
    b"PASSWD",
    b"PASSPHRASE",
    b"TOKEN",
    b"SECRET",
    b"CREDENTIAL",
    b"COOKIE",
    b"AUTH",
    b"SESSION_ID",
    b"SESSIONID",
    b"SESSID",
];

const fn folded(byte: u8) -> u8 {
    if byte.is_ascii_alphanumeric() {
        byte.to_ascii_uppercase()
    } else {
        b'_'
    }
}

/// `DB_PASS`, `--pass`, `API_KEY`, `--private-key`, `APIKEY`.
fn is_secret_last_word(name: &[u8]) -> bool {
    let end = name.iter().rposition(u8::is_ascii_alphanumeric).map_or(0, |at| at + 1);
    let name = name.get(..end).unwrap_or_default();
    let start = name
        .iter()
        .rposition(|byte| !byte.is_ascii_alphanumeric())
        .map_or(0, |at| at + 1);
    let word = name.get(start..).unwrap_or_default();
    word.eq_ignore_ascii_case(b"PASS")
        || word
            .len()
            .checked_sub(3)
            .and_then(|at| word.get(at..))
            .is_some_and(|tail| tail.eq_ignore_ascii_case(b"KEY"))
}

/// Whether a variable, flag or `key=` names a secret; case and `-`/`.`/`_` don't matter.
#[must_use]
pub fn is_secret_name(name: &str) -> bool {
    let name = name.as_bytes();
    is_secret_last_word(name)
        || SECRET_WORDS.iter().any(|word| {
            name.windows(word.len()).any(|window| {
                window
                    .iter()
                    .zip(word.iter())
                    .all(|(&byte, &want)| folded(byte) == want)
            })
        })
}

/// psport's `is_secret_value`: a JWT or an `Authorization` header, whatever it is named.
fn is_secret_value(value: &str) -> bool {
    value.trim_start().starts_with("eyJ") || !bearer_spans(value).is_empty()
}

/// Whether a variable is logged as [`REDACTED`] whole; the rest goes through [`text`].
#[must_use]
pub fn is_secret_env(name: &str, value: &str) -> bool {
    is_secret_name(name) || is_secret_value(value)
}

/// A command's arguments as logged: the value of a secret flag, after `=` or as the argument
/// after it, and whatever [`text`] hides in the rest.
#[must_use]
pub fn args<I>(args: I) -> Vec<String>
where
    I: IntoIterator,
    I::Item: AsRef<str>,
{
    let mut is_value_secret = false;
    args.into_iter()
        .map(|arg| {
            let arg = arg.as_ref();
            let logged = if is_value_secret || is_secret_value(arg) {
                REDACTED.to_owned()
            } else {
                text(arg).into_owned()
            };
            is_value_secret = arg.starts_with('-') && !arg.contains('=') && is_secret_name(arg);
            logged
        })
        .collect()
}

type Pass = fn(&str) -> Vec<Range<usize>>;

const SECRET_PASSES: [Pass; 4] = [url_spans, pair_spans, bearer_spans, jwt_spans];
const ID_PASSES: [Pass; 2] = [id_pair_spans, friend_spans];
const ADDRESS_PASSES: [Pass; 3] = [mac_spans, ipv6_spans, ipv4_spans];

/// Free text: URL credentials and queries, `name=value` with a secret name, `Bearer` tokens and
/// JWTs. Borrowed when nothing matched, as it is for almost every line a port prints.
#[must_use]
pub fn text(text: &str) -> Cow<'_, str> {
    replaced(Cow::Borrowed(text), &SECRET_PASSES, REDACTED).0
}

/// [`text`] for what leaves the machine (a report's log, the clipboard), counted in `hidden`.
///
/// After the secrets, account ids and other players' NPIDs, then every IP and MAC address but
/// loopback and wildcards as [`ADDRESS`]. The local logs keep them, since they tell which account,
/// friend, server or LAN peer a run dealt with.
#[must_use]
pub fn outgoing<'a>(text: &'a str, hidden: &mut Hidden) -> Cow<'a, str> {
    let (text, values) = replaced(Cow::Borrowed(text), &SECRET_PASSES, REDACTED);
    let (text, ids) = replaced(text, &ID_PASSES, REDACTED);
    let (text, addresses) = replaced(text, &ADDRESS_PASSES, ADDRESS);
    hidden.values += values;
    hidden.ids += ids;
    hidden.addresses += addresses;
    text
}

fn replaced<'a>(text: Cow<'a, str>, passes: &[Pass], with: &str) -> (Cow<'a, str>, usize) {
    passes.iter().fold((text, 0), |(text, count), &pass| {
        let (text, replaced) = apply(text, pass, with);
        (text, count + replaced)
    })
}

/// Each pass's spans come sorted and apart, so one copy replaces them all. A span that already
/// holds `with` is left alone and not counted, so scrubbing scrubbed text changes nothing.
fn apply<'a>(text: Cow<'a, str>, pass: Pass, with: &str) -> (Cow<'a, str>, usize) {
    let mut spans = pass(&text);
    spans.retain(|span| text.get(span.start..span.end) != Some(with));
    if spans.is_empty() {
        return (text, 0);
    }
    let count = spans.len();
    let mut out = String::with_capacity(text.len());
    let mut copied = 0;
    for span in spans {
        out.push_str(text.get(copied..span.start).unwrap_or_default());
        out.push_str(with);
        copied = span.end;
    }
    out.push_str(text.get(copied..).unwrap_or_default());
    (Cow::Owned(out), count)
}

const fn ends_url(ch: char) -> bool {
    ch.is_whitespace() || matches!(ch, '"' | '\'' | '<' | '>' | '`')
}

/// The userinfo before a URL's host and everything after its `?`. The authority ends at the first
/// `/`, `?` or `#`, at or past where the URL parser ends it, so no userinfo it would read is missed.
fn url_spans(text: &str) -> Vec<Range<usize>> {
    let mut spans: Vec<Range<usize>> = Vec::new();
    for (at, separator) in text.match_indices("://") {
        let start = at + separator.len();
        if spans.last().is_some_and(|last| start <= last.end) {
            continue;
        }
        let url = text.get(start..).unwrap_or_default();
        let url = url.get(..url.find(ends_url).unwrap_or(url.len())).unwrap_or_default();
        let authority = url.find(['/', '?', '#']).unwrap_or(url.len());
        if let Some(userinfo) = url.get(..authority).and_then(|authority| authority.rfind('@')) {
            spans.push(start..start + userinfo);
        }
        if let Some(query) = url.find('?').filter(|&query| query + 1 < url.len()) {
            spans.push(start + query + 1..start + url.len());
        }
    }
    spans.retain(|span| !span.is_empty());
    spans
}

const fn is_name_char(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.')
}

const fn ends_value(ch: char) -> bool {
    ch.is_whitespace() || matches!(ch, '&' | ',' | ';' | '"' | '\'' | ')' | ']' | '}')
}

/// The value of `name=value`, `--name=value` or `name="value"` when the name is a secret's.
fn pair_spans(text: &str) -> Vec<Range<usize>> {
    named_value_spans(text, is_secret_name)
}

fn named_value_spans(text: &str, is_name: fn(&str) -> bool) -> Vec<Range<usize>> {
    let mut spans: Vec<Range<usize>> = Vec::new();
    for (at, _) in text.match_indices('=') {
        if spans.last().is_some_and(|last| at < last.end) {
            continue;
        }
        let name = text
            .get(..at)
            .and_then(|head| head.rsplit(|ch: char| !is_name_char(ch)).next())
            .unwrap_or_default();
        if name.is_empty() || !is_name(name) {
            continue;
        }
        if let Some(span) = value_span(text, at + 1) {
            spans.push(span);
        }
    }
    spans
}

/// The credentials after `Bearer `, in any case, as an `Authorization` header carries them.
fn bearer_spans(text: &str) -> Vec<Range<usize>> {
    const BEARER: &[u8] = b"bearer ";
    let mut spans: Vec<Range<usize>> = Vec::new();
    for (at, window) in text.as_bytes().windows(BEARER.len()).enumerate() {
        if !window.eq_ignore_ascii_case(BEARER) || spans.last().is_some_and(|last| at < last.end) {
            continue;
        }
        let rest = text.get(at + BEARER.len()..).unwrap_or_default();
        let start = text.len() - rest.trim_start_matches(' ').len();
        let token = text.get(start..).unwrap_or_default();
        let span = start..start + token.find(ends_url).unwrap_or(token.len());
        if !span.is_empty() {
            spans.push(span);
        }
    }
    spans
}

const fn is_base64url(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_')
}

/// A JWT standing alone in text: `eyJ` (base64 of `{"`) and dot-separated base64url parts.
fn jwt_spans(text: &str) -> Vec<Range<usize>> {
    let mut spans: Vec<Range<usize>> = Vec::new();
    for (at, _) in text.match_indices("eyJ") {
        let is_start = text
            .get(..at)
            .and_then(|head| head.chars().next_back())
            .is_none_or(|ch| !is_base64url(ch) && ch != '.');
        if !is_start || spans.last().is_some_and(|last| at < last.end) {
            continue;
        }
        let token = text.get(at..).unwrap_or_default();
        let len = token
            .find(|ch: char| !is_base64url(ch) && ch != '.')
            .unwrap_or(token.len());
        if token.get(..len).is_some_and(|token| token.contains('.')) {
            spans.push(at..at + len);
        }
    }
    spans
}

/// `npid=`, `fromNpid=`, `onlineId=`, `accountId=`, `account_id=`, bare or quoted, in any case:
/// how psport names a player and decanter a storefront account.
fn is_id_name(name: &str) -> bool {
    [b"NPID".as_slice(), b"ONLINEID", b"ACCOUNTID"].into_iter().any(|word| {
        let mut letters = name.bytes().rev().filter(u8::is_ascii_alphanumeric);
        word.iter()
            .rev()
            .all(|&want| letters.next().is_some_and(|byte| byte.to_ascii_uppercase() == want))
    })
}

fn id_pair_spans(text: &str) -> Vec<Range<usize>> {
    named_value_spans(text, is_id_name)
}

/// The value starting at `start`: inside its quotes when quoted, else up to [`ends_value`].
fn value_span(text: &str, start: usize) -> Option<Range<usize>> {
    let value = text.get(start..)?;
    let span = match value.chars().next() {
        Some(quote @ ('"' | '\'')) => {
            let inner = value.get(1..).unwrap_or_default();
            start + 1..start + 1 + inner.find(quote).unwrap_or(inner.len())
        }
        _ => start..start + value.find(ends_value).unwrap_or(value.len()),
    };
    (!span.is_empty()).then_some(span)
}

/// The quoted name after a friend event (`FriendNew 'name'`, `FriendQuery from 'name'`,
/// `AddFriend 'name'`): psport logs other players' NPIDs that way, and they are nobody's to share.
fn friend_spans(text: &str) -> Vec<Range<usize>> {
    let mut spans: Vec<Range<usize>> = Vec::new();
    for (at, _) in text.match_indices("Friend") {
        let word_end = text
            .get(at..)
            .and_then(|tail| tail.find(|ch: char| !ch.is_ascii_alphanumeric()))
            .map_or(text.len(), |len| at + len);
        let rest = text.get(word_end..).unwrap_or_default();
        let rest = rest.strip_prefix(" from").unwrap_or(rest);
        let Some(quoted) = rest.strip_prefix(" '") else {
            continue;
        };
        let start = text.len() - quoted.len() - 1;
        if spans.last().is_some_and(|last| start < last.end) {
            continue;
        }
        if let Some(span) = value_span(text, start) {
            spans.push(span);
        }
    }
    spans
}

/// The maximal runs of ASCII bytes `is_part` accepts; a run never splits a character.
fn runs(text: &str, is_part: fn(u8) -> bool) -> impl Iterator<Item = Range<usize>> + '_ {
    let bytes = text.as_bytes();
    let mut at = 0;
    core::iter::from_fn(move || {
        let start = at + bytes.get(at..)?.iter().position(|&byte| is_part(byte))?;
        let rest = bytes.get(start..).unwrap_or_default();
        at = start + rest.iter().position(|&byte| !is_part(byte)).unwrap_or(rest.len());
        Some(start..at)
    })
}

/// Not glued to a word on either side, so `v1.2.3.4` or `build1.2.3.4x` stay versions.
fn stands_alone(text: &str, span: &Range<usize>) -> bool {
    let bytes = text.as_bytes();
    let before = span.start.checked_sub(1).and_then(|at| bytes.get(at));
    !before.is_some_and(u8::is_ascii_alphanumeric) && !bytes.get(span.end).is_some_and(u8::is_ascii_alphanumeric)
}

/// `aa:bb:cc:dd:ee:ff` or with `-`; the all-zero address names no device.
fn mac_spans(text: &str) -> Vec<Range<usize>> {
    runs(text, |byte| byte.is_ascii_hexdigit() || matches!(byte, b':' | b'-'))
        .filter(|run| run.len() == 17 && stands_alone(text, run))
        .filter(|run| {
            let mac = text.as_bytes().get(run.start..run.end).unwrap_or_default();
            let separator = mac.get(2).copied().unwrap_or_default();
            matches!(separator, b':' | b'-')
                && mac.iter().enumerate().all(|(at, &byte)| {
                    if at % 3 == 2 {
                        byte == separator
                    } else {
                        byte.is_ascii_hexdigit()
                    }
                })
                && mac.iter().any(|&byte| byte.is_ascii_hexdigit() && byte != b'0')
        })
        .collect()
}

/// Whatever the standard parser takes as `IPv6` out of a run of hex digits, `:` and `.` (so
/// `::ffff:10.0.0.1` goes whole), with two colons and a digit at least: C++ scopes and words
/// such as `add::bed` are not addresses, and `12:34:56.789` does not parse.
fn ipv6_spans(text: &str) -> Vec<Range<usize>> {
    runs(text, |byte| byte.is_ascii_hexdigit() || matches!(byte, b':' | b'.'))
        .filter(|run| stands_alone(text, run))
        .filter_map(|mut run| {
            let candidate = text.get(run.clone())?;
            let trimmed = candidate.trim_end_matches('.');
            let trimmed = if trimmed.ends_with(':') && !trimmed.ends_with("::") {
                trimmed.get(..trimmed.len() - 1)?
            } else {
                trimmed
            };
            run.end = run.start + trimmed.len();
            let is_shaped = trimmed.matches(':').nth(1).is_some() && trimmed.bytes().any(|byte| byte.is_ascii_digit());
            let address = trimmed.parse::<Ipv6Addr>().ok().filter(|_| is_shaped)?;
            (!address.is_loopback() && !address.is_unspecified()).then_some(run)
        })
        .collect()
}

/// Dotted quads other than loopback, `0.x.x.x` and `255.x.x.x` (wildcards, broadcasts, masks):
/// public and LAN addresses alike, since a LAN address still maps a home network.
fn ipv4_spans(text: &str) -> Vec<Range<usize>> {
    runs(text, |byte| byte.is_ascii_digit() || byte == b'.')
        .filter(|run| stands_alone(text, run))
        .filter_map(|mut run| {
            let trimmed = text.get(run.clone())?.trim_end_matches('.');
            run.end = run.start + trimmed.len();
            let address = trimmed.parse::<Ipv4Addr>().ok()?;
            let [first, ..] = address.octets();
            (!address.is_loopback() && first != 0 && first != 255).then_some(run)
        })
        .collect()
}
