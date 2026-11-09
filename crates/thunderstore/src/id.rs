// SPDX-License-Identifier: GPL-3.0-or-later
//! The names Thunderstore's answers carry, checked once where they enter: communities, packages,
//! versions, dependency strings, URLs and display text. Package names follow the package format
//! page (letters, digits and `_`, at most 128) and dependency strings the analysis' strict
//! `^[A-Za-z0-9_]+-[A-Za-z0-9_]+-\d+\.\d+\.\d+$`, so a name never holds a `-` and can name a folder.

use core::fmt;
use core::str::FromStr;

use crate::{Error, Result};

const MAX_NAME: usize = 128;
const MAX_COMMUNITY: usize = 64;
const MAX_URL: usize = 2048;
/// The domain whose hosts decanter talks to and downloads from; anything else is refused.
pub(crate) const DOMAIN: &str = "thunderstore.io";

/// A community's identifier, the `{community}` of its URLs.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CommunityId(String);

impl CommunityId {
    #[must_use]
    #[inline]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl FromStr for CommunityId {
    type Err = Error;

    #[inline]
    fn from_str(text: &str) -> Result<Self> {
        let is_valid = (1..=MAX_COMMUNITY).contains(&text.len())
            && text
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'));
        if is_valid {
            Ok(Self(text.to_owned()))
        } else {
            Err(Error::InvalidCommunity(text.to_owned()))
        }
    }
}

impl fmt::Display for CommunityId {
    #[inline]
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A package: its team (namespace) and its name, `Team-Name` together.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PackageId {
    namespace: String,
    name: String,
}

impl PackageId {
    /// # Errors
    ///
    /// [`Error::InvalidPackage`] when either part is not a package name.
    #[inline]
    pub fn new(namespace: &str, name: &str) -> Result<Self> {
        for part in [namespace, name] {
            if !is_name(part) {
                return Err(Error::InvalidPackage(part.to_owned()));
            }
        }
        Ok(Self {
            namespace: namespace.to_owned(),
            name: name.to_owned(),
        })
    }

    #[must_use]
    #[inline]
    pub fn namespace(&self) -> &str {
        &self.namespace
    }

    #[must_use]
    #[inline]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The name as people write it: Thunderstore's names stand `_` for a space.
    #[must_use]
    #[inline]
    pub fn display_name(&self) -> String {
        self.name.replace('_', " ")
    }
}

impl FromStr for PackageId {
    type Err = Error;

    /// `Team-Name`, as a mod's origin stores it.
    #[inline]
    fn from_str(text: &str) -> Result<Self> {
        let (namespace, name) = text
            .split_once('-')
            .ok_or_else(|| Error::InvalidPackage(text.to_owned()))?;
        Self::new(namespace, name)
    }
}

impl fmt::Display for PackageId {
    #[inline]
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}-{}", self.namespace, self.name)
    }
}

/// `major.minor.patch`, the manifest's `version_number`; the highest wins.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct VersionNumber {
    major: u32,
    minor: u32,
    patch: u32,
}

impl FromStr for VersionNumber {
    type Err = Error;

    #[inline]
    fn from_str(text: &str) -> Result<Self> {
        let invalid = || Error::InvalidDependency(text.to_owned());
        let mut parts = text.split('.');
        let mut next = || -> Result<u32> {
            let part = parts.next().ok_or_else(invalid)?;
            if part.is_empty() || part.len() > 9 || !part.bytes().all(|byte| byte.is_ascii_digit()) {
                return Err(invalid());
            }
            part.parse().map_err(|_err| invalid())
        };
        let version = Self {
            major: next()?,
            minor: next()?,
            patch: next()?,
        };
        if parts.next().is_some() {
            return Err(invalid());
        }
        Ok(version)
    }
}

impl fmt::Display for VersionNumber {
    #[inline]
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

/// One entry of a version's `dependencies`: `Team-Name-1.2.3`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub struct Dependency {
    pub package: PackageId,
    pub version: VersionNumber,
}

impl FromStr for Dependency {
    type Err = Error;

    #[inline]
    fn from_str(text: &str) -> Result<Self> {
        let invalid = || Error::InvalidDependency(text.to_owned());
        let mut parts = text.splitn(3, '-');
        let (Some(namespace), Some(name), Some(version)) = (parts.next(), parts.next(), parts.next()) else {
            return Err(invalid());
        };
        Ok(Self {
            package: PackageId::new(namespace, name).map_err(|_err| invalid())?,
            version: version.parse().map_err(|_err| invalid())?,
        })
    }
}

impl fmt::Display for Dependency {
    #[inline]
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}-{}", self.package, self.version)
    }
}

fn is_name(text: &str) -> bool {
    (1..=MAX_NAME).contains(&text.len()) && text.bytes().all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
}

/// Whether `host` is Thunderstore's own or one of its subdomains (its CDN).
pub(crate) fn is_thunderstore_host(host: &str) -> bool {
    host.eq_ignore_ascii_case(DOMAIN)
        || host
            .len()
            .checked_sub(DOMAIN.len().saturating_add(1))
            .and_then(|cut| host.get(cut..))
            .is_some_and(|tail| tail.eq_ignore_ascii_case(&format!(".{DOMAIN}")))
}

/// `text` as a URL decanter may ask: HTTPS on a Thunderstore host, on its default port, without
/// credentials.
pub(crate) fn checked_url(text: &str) -> Option<url::Url> {
    if text.len() > MAX_URL {
        return None;
    }
    let url = url::Url::parse(text).ok()?;
    let is_allowed = url.scheme() == "https"
        && url.username().is_empty()
        && url.password().is_none()
        && url.port().is_none()
        && url.host_str().is_some_and(is_thunderstore_host);
    is_allowed.then_some(url)
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
