//! Secrets out of what decanter logs about the processes it starts and the lines they print.
//!
//! One policy for the run log header, failure records and the debug log. Only what is logged goes
//! through it; the environment and arguments a child gets stay as they were. A value is replaced
//! whole by [`REDACTED`]; names stay, so a log still says what was set.

use core::ops::Range;
use std::borrow::Cow;

pub const REDACTED: &str = "<redacted>";

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

/// Free text: URL credentials and queries, `name=value` with a secret name, `Bearer` tokens and
/// JWTs. Borrowed when nothing matched, as it is for almost every line a port prints.
#[must_use]
pub fn text(text: &str) -> Cow<'_, str> {
    const PASSES: [Pass; 4] = [url_spans, pair_spans, bearer_spans, jwt_spans];
    PASSES.into_iter().fold(Cow::Borrowed(text), apply)
}

/// Each pass's spans come sorted and apart, so one copy replaces them all.
fn apply(text: Cow<'_, str>, pass: Pass) -> Cow<'_, str> {
    let spans = pass(&text);
    if spans.is_empty() {
        return text;
    }
    let mut out = String::with_capacity(text.len());
    let mut copied = 0;
    for span in spans {
        out.push_str(text.get(copied..span.start).unwrap_or_default());
        out.push_str(REDACTED);
        copied = span.end;
    }
    out.push_str(text.get(copied..).unwrap_or_default());
    Cow::Owned(out)
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
    let mut spans: Vec<Range<usize>> = Vec::new();
    for (at, _) in text.match_indices('=') {
        if spans.last().is_some_and(|last| at < last.end) {
            continue;
        }
        let name = text
            .get(..at)
            .and_then(|head| head.rsplit(|ch: char| !is_name_char(ch)).next())
            .unwrap_or_default();
        if name.is_empty() || !is_secret_name(name) {
            continue;
        }
        let start = at + 1;
        let value = text.get(start..).unwrap_or_default();
        let span = match value.chars().next() {
            Some(quote @ ('"' | '\'')) => {
                let inner = value.get(1..).unwrap_or_default();
                start + 1..start + 1 + inner.find(quote).unwrap_or(inner.len())
            }
            _ => start..start + value.find(ends_value).unwrap_or(value.len()),
        };
        if !span.is_empty() {
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
