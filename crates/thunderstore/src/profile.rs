//! A profile code of Thunderstore's mod managers, read as the list of packages it names. The code is
//! a UUID; `legacyprofile/get/<code>/` answers `#r2modman` and the base64 of a zip whose `export.r2x`
//! is YAML (`profileName` and `mods`, each with `name`, `version` as `major`/`minor`/`patch` and
//! `enabled`), as the MIT-licensed r2modman reads it. No page documents the endpoint, so a shape that
//! differs is refused rather than guessed at. The zip's other files, r2modman's configuration, are
//! never read: a profile here is its package list.

use core::fmt;
use core::str::FromStr;
use std::io::Read;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;

use crate::http::Client;
use crate::id::{DOMAIN, PackageId, VersionNumber};
use crate::{Error, Result};

const CALL: &str = "the profile";
const PREFIX: &str = "#r2modman";
/// The configuration files of a profile ride in the same zip, so the cap is generous.
const MAX_DOWNLOAD: u64 = 64 * 1024 * 1024;
const MAX_LIST_BYTES: u64 = 2 * 1024 * 1024;
const MAX_MODS: usize = 1000;
const MAX_NAME_CHARS: usize = 200;
const LIST_FILE: &str = "export.r2x";

/// The UUID a profile is shared by.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfileCode(String);

impl FromStr for ProfileCode {
    type Err = Error;

    #[inline]
    fn from_str(text: &str) -> Result<Self> {
        let text = text.trim();
        let groups: Vec<&str> = text.split('-').collect();
        let is_uuid = groups.iter().map(|group| group.len()).eq([8, 4, 4, 4, 12])
            && groups
                .iter()
                .all(|group| group.bytes().all(|byte| byte.is_ascii_hexdigit()));
        if is_uuid {
            Ok(Self(text.to_ascii_lowercase()))
        } else {
            Err(Error::InvalidProfileCode(text.chars().take(MAX_NAME_CHARS).collect()))
        }
    }
}

impl fmt::Display for ProfileCode {
    #[inline]
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct Profile {
    pub name: String,
    /// In the profile's own order, which is its load order.
    pub mods: Vec<ProfileMod>,
}

#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct ProfileMod {
    pub package: PackageId,
    pub version: VersionNumber,
    pub enabled: bool,
}

/// The profile `code` names.
///
/// # Errors
///
/// As a request ([`Error::NotFound`] for a code that does not exist or expired), and
/// [`Error::Malformed`] when what comes back is not a profile of the shape above.
#[inline]
pub async fn profile(client: &Client, code: &ProfileCode) -> Result<Profile> {
    let url = url::Url::parse(&format!("https://{DOMAIN}/api/experimental/legacyprofile/get/{code}/"))
        .map_err(|_err| Error::malformed(CALL, "the address is not a URL"))?;
    let body = client.get_bytes(url, CALL, MAX_DOWNLOAD).await?;
    read_profile(&body)
}

fn read_profile(body: &[u8]) -> Result<Profile> {
    let bad = |reason: &str| Error::malformed(CALL, reason);
    let text = core::str::from_utf8(body).map_err(|_err| bad("not text"))?;
    let encoded = text
        .trim()
        .strip_prefix(PREFIX)
        .ok_or_else(|| bad("not a profile code's file"))?;
    let zipped = STANDARD
        .decode(encoded.trim().as_bytes())
        .map_err(|_err| bad("the profile is not base64"))?;
    let mut archive =
        zip::ZipArchive::new(std::io::Cursor::new(zipped)).map_err(|_err| bad("the profile is not a zip"))?;
    let entry = archive
        .by_name(LIST_FILE)
        .map_err(|_err| bad("the profile has no export.r2x"))?;
    let mut list = String::new();
    entry
        .take(MAX_LIST_BYTES.saturating_add(1))
        .read_to_string(&mut list)
        .map_err(|_err| bad("export.r2x is not text"))?;
    if u64::try_from(list.len()).unwrap_or(u64::MAX) > MAX_LIST_BYTES {
        return Err(bad("export.r2x is too large"));
    }
    parse_list(&list)
}

/// The block-style YAML `export.r2x` is written in; any shape beyond it is refused.
fn parse_list(text: &str) -> Result<Profile> {
    let bad = |reason: &str| Error::malformed(CALL, reason);
    let mut name = None;
    let mut mods: Vec<Draft> = Vec::new();
    let mut in_mods = false;
    let mut in_version = false;
    for line in text.lines().map(str::trim_end).filter(|line| !line.trim().is_empty()) {
        let indent = line.len().saturating_sub(line.trim_start().len());
        let content = line.trim_start();
        if indent == 0 {
            in_mods = false;
            let (key, value) = split(content).ok_or_else(|| bad("a line is not `key: value`"))?;
            match key {
                "profileName" => name = Some(scalar(value).ok_or_else(|| bad("profileName is empty"))?),
                "mods" => in_mods = value.is_empty(),
                _ => {}
            }
            continue;
        }
        if !in_mods {
            continue;
        }
        let (item, content) = match content.strip_prefix("- ") {
            Some(rest) => (true, rest.trim_start()),
            None => (false, content),
        };
        if item {
            if mods.len() >= MAX_MODS {
                return Err(bad("too many mods"));
            }
            mods.push(Draft::default());
            in_version = false;
        }
        let Some(draft) = mods.last_mut() else {
            return Err(bad("a mod's line comes before its list"));
        };
        let (key, value) = split(content).ok_or_else(|| bad("a line is not `key: value`"))?;
        match (key, in_version) {
            ("name", _) => draft.name = Some(scalar(value).ok_or_else(|| bad("a mod has no name"))?),
            ("enabled", _) => draft.enabled = !matches!(scalar(value).as_deref(), Some("false")),
            ("version", _) => {
                in_version = value.is_empty();
                if !value.is_empty() {
                    draft.version = Some(value.trim_matches(['"', '\'']).to_owned());
                }
            }
            ("major", true) => draft.major = scalar(value),
            ("minor", true) => draft.minor = scalar(value),
            ("patch", true) => draft.patch = scalar(value),
            _ => {}
        }
    }
    let name = name.ok_or_else(|| bad("export.r2x has no profileName"))?;
    let mods = mods.into_iter().map(Draft::finish).collect::<Result<Vec<_>>>()?;
    Ok(Profile {
        name: name
            .chars()
            .filter(|character| !character.is_control())
            .take(MAX_NAME_CHARS)
            .collect(),
        mods,
    })
}

#[derive(Default)]
struct Draft {
    name: Option<String>,
    version: Option<String>,
    major: Option<String>,
    minor: Option<String>,
    patch: Option<String>,
    enabled: bool,
}

impl Draft {
    fn finish(self) -> Result<ProfileMod> {
        let bad = |reason: &str| Error::malformed(CALL, reason);
        let name = self.name.ok_or_else(|| bad("a mod has no name"))?;
        let version = match (self.version, self.major, self.minor, self.patch) {
            (Some(version), ..) => version,
            (None, Some(major), Some(minor), Some(patch)) => format!("{major}.{minor}.{patch}"),
            _ => return Err(bad("a mod has no version")),
        };
        Ok(ProfileMod {
            package: name.parse().map_err(|_err| bad("a mod's name is not Team-Name"))?,
            version: version
                .parse::<VersionNumber>()
                .map_err(|_err| bad("a mod's version is not major.minor.patch"))?,
            enabled: self.enabled,
        })
    }
}

fn split(content: &str) -> Option<(&str, &str)> {
    let (key, value) = content.split_once(':')?;
    Some((key.trim(), value.trim()))
}

fn scalar(value: &str) -> Option<String> {
    let value = value.trim_matches(['"', '\'']);
    (!value.is_empty()).then(|| value.to_owned())
}
