// SPDX-License-Identifier: GPL-3.0-or-later
//! The names Nexus' answers and the user's pastes carry, checked once where they enter: game
//! domains, mod and file numbers, URLs and display text.

use core::fmt;
use core::str::FromStr;

use crate::{Error, Result};

const MAX_DOMAIN: usize = 64;
/// Nexus' numbers are counters; twelve digits leave decades of room and keep a paste short.
const MAX_DIGITS: usize = 12;
const MAX_URL: usize = 4096;
pub(crate) const API_HOST: &str = "api.nexusmods.com";
const SITE_HOST: &str = "www.nexusmods.com";
/// The domains a download link may point into: Nexus' own and its CDN's, as far as the analysis'
/// secondary sources show (section 1); a link anywhere else is refused, not followed.
const DOWNLOAD_DOMAINS: [&str; 2] = ["nexusmods.com", "nexus-cdn.com"];

/// A game's `domain_name`, the `{game}` of every API path and of its pages.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct GameDomain(String);

impl GameDomain {
    #[must_use]
    #[inline]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl FromStr for GameDomain {
    type Err = Error;

    #[inline]
    fn from_str(text: &str) -> Result<Self> {
        let is_valid = (1..=MAX_DOMAIN).contains(&text.len())
            && text
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'-' | b'_'));
        if is_valid {
            Ok(Self(text.to_owned()))
        } else {
            Err(Error::InvalidDomain(text.to_owned()))
        }
    }
}

impl fmt::Display for GameDomain {
    #[inline]
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A mod's number within its game, the `{mod}` of its page.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct NexusModId(u64);

/// One uploaded file of a mod: the version a user installs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FileId(u64);

impl NexusModId {
    /// `None` for 0 or past twelve digits, which no Nexus number is.
    #[must_use]
    #[inline]
    pub const fn new(value: u64) -> Option<Self> {
        if is_number(value) { Some(Self(value)) } else { None }
    }
}

impl FileId {
    /// `None` for 0 or past twelve digits, which no Nexus number is.
    #[must_use]
    #[inline]
    pub const fn new(value: u64) -> Option<Self> {
        if is_number(value) { Some(Self(value)) } else { None }
    }
}

const fn is_number(value: u64) -> bool {
    value > 0 && value < 1_000_000_000_000
}

fn parse_number(text: &str) -> Result<u64> {
    let invalid = || Error::InvalidReference(text.to_owned());
    if text.is_empty() || text.len() > MAX_DIGITS || !text.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(invalid());
    }
    text.parse::<u64>()
        .ok()
        .filter(|&value| is_number(value))
        .ok_or_else(invalid)
}

impl FromStr for NexusModId {
    type Err = Error;

    #[inline]
    fn from_str(text: &str) -> Result<Self> {
        parse_number(text).map(Self)
    }
}

impl FromStr for FileId {
    type Err = Error;

    #[inline]
    fn from_str(text: &str) -> Result<Self> {
        parse_number(text).map(Self)
    }
}

impl fmt::Display for NexusModId {
    #[inline]
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl fmt::Display for FileId {
    #[inline]
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// A mod the user pasted: its number, or the address of its page on the site, which must be of
/// `domain`, the game the browser shows.
///
/// # Errors
///
/// [`Error::InvalidReference`] for anything else, a page of another game included.
#[inline]
pub fn parse_mod_reference(pasted: &str, domain: &GameDomain) -> Result<NexusModId> {
    let text = pasted.trim();
    let invalid = || Error::InvalidReference(display_text(text, 200));
    if text.bytes().all(|byte| byte.is_ascii_digit()) {
        return text.parse();
    }
    if text.len() > MAX_URL {
        return Err(invalid());
    }
    let url = url::Url::parse(text).map_err(|_err| invalid())?;
    let is_site = matches!(url.scheme(), "https" | "http")
        && url
            .host_str()
            .is_some_and(|host| host.eq_ignore_ascii_case(SITE_HOST) || host.eq_ignore_ascii_case("nexusmods.com"));
    if !is_site {
        return Err(invalid());
    }
    let mut segments = url.path_segments().ok_or_else(invalid)?;
    match (segments.next(), segments.next(), segments.next()) {
        (Some(game), Some("mods"), Some(number)) if game == domain.as_str() => number.parse().map_err(|_err| invalid()),
        _ => Err(invalid()),
    }
}

/// The mod's page on the site, files tab: where a free account downloads it by hand.
#[must_use]
#[inline]
pub fn mod_page(domain: &GameDomain, id: NexusModId) -> String {
    format!("https://{SITE_HOST}/{domain}/mods/{id}?tab=files")
}

pub(crate) fn api_url(path: &str) -> Option<url::Url> {
    url::Url::parse(&format!("https://{API_HOST}{path}")).ok()
}

fn is_under(host: &str, domain: &str) -> bool {
    host.eq_ignore_ascii_case(domain)
        || host
            .len()
            .checked_sub(domain.len().saturating_add(1))
            .and_then(|cut| host.get(cut..))
            .is_some_and(|tail| {
                tail.starts_with('.') && tail.get(1..).is_some_and(|rest| rest.eq_ignore_ascii_case(domain))
            })
}

pub(crate) fn is_download_host(host: &str) -> bool {
    DOWNLOAD_DOMAINS.iter().any(|domain| is_under(host, domain))
}

/// `text` as a download link decanter may follow: HTTPS on a Nexus or Nexus CDN host, default
/// port, no credentials in it.
pub(crate) fn checked_download_url(text: &str) -> Option<url::Url> {
    if text.len() > MAX_URL {
        return None;
    }
    let url = url::Url::parse(text).ok()?;
    let is_allowed = url.scheme() == "https"
        && url.username().is_empty()
        && url.password().is_none()
        && url.port().is_none()
        && url.host_str().is_some_and(is_download_host);
    is_allowed.then_some(url)
}

/// An image address from an answer, kept only on a Nexus host over HTTPS so the app never fetches
/// anything else for a listing.
pub(crate) fn checked_picture(text: &str) -> Option<String> {
    checked_download_url(text).map(String::from)
}

/// Text from an answer shown as it is: control characters become spaces and it is cut at
/// `max_chars`, since it is rendered as plain text and never interpreted.
pub(crate) fn display_text(text: &str, max_chars: usize) -> String {
    text.trim()
        .chars()
        .take(max_chars)
        .map(|character| if character.is_control() { ' ' } else { character })
        .collect()
}

/// The date part of an ISO 8601 timestamp, the only part shown.
pub(crate) fn date(text: &str) -> Option<String> {
    let day = text.get(..10)?;
    let is_date = day.bytes().enumerate().all(|(index, byte)| {
        if index == 4 || index == 7 {
            byte == b'-'
        } else {
            byte.is_ascii_digit()
        }
    });
    is_date.then(|| day.to_owned())
}
