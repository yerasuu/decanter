// SPDX-License-Identifier: GPL-3.0-or-later
//! A game's mods as the v1 API offers them: the latest added, trending and latest updated lists
//! (`/v1/games/{game}/mods/{latest_added,trending,latest_updated}.json`, about ten each) and one
//! mod by number (`/v1/games/{game}/mods/{mod}.json`); all secondary, analysis section 1. v1 has no
//! search by name, so the app filters what these lists hold and opens any other mod by its number
//! or page address. A mod that does not hold up is skipped and counted.

use decanter_mods::ModCredits;
use serde::Deserialize;

use crate::http::Client;
use crate::id::{GameDomain, NexusModId, api_url, checked_picture, date, display_text};
use crate::{Error, Result};

const MAX_LIST_BYTES: u64 = 4 << 20;
const MAX_MOD_BYTES: u64 = 2 << 20;
const MAX_LIST: usize = 200;
const MAX_NAME_CHARS: usize = 200;
const MAX_SUMMARY_CHARS: usize = 600;
const MAX_FIELD_CHARS: usize = 100;
const MAX_URL: usize = 2048;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Feed {
    Latest,
    Trending,
    Updated,
}

impl Feed {
    const fn path(self) -> &'static str {
        match self {
            Self::Latest => "latest_added",
            Self::Trending => "trending",
            Self::Updated => "latest_updated",
        }
    }

    const fn call(self) -> &'static str {
        match self {
            Self::Latest => "the latest mods",
            Self::Trending => "the trending mods",
            Self::Updated => "the updated mods",
        }
    }
}

#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct ModInfo {
    pub id: NexusModId,
    pub name: String,
    /// Plain text; the long description is `BBCode` and not shown.
    pub summary: String,
    /// HTTPS on a Nexus host.
    pub picture: Option<String>,
    /// The mod's own version string, free text the author types.
    pub version: Option<String>,
    pub author: Option<String>,
    /// As the answer gives it; only [`ModInfo::credits`] reads it, which checks it.
    pub author_page: Option<String>,
    pub endorsements: u64,
    /// `YYYY-MM-DD`.
    pub updated: Option<String>,
    pub has_adult_content: bool,
    /// Whether Nexus shows it: hidden, removed or unpublished mods come in lists too.
    pub is_available: bool,
}

#[derive(Deserialize)]
struct RawMod {
    mod_id: u64,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    summary: Option<String>,
    #[serde(default)]
    picture_url: Option<String>,
    #[serde(default)]
    version: Option<String>,
    #[serde(default)]
    author: Option<String>,
    #[serde(default)]
    uploaded_users_profile_url: Option<String>,
    #[serde(default)]
    endorsement_count: Option<u64>,
    #[serde(default)]
    updated_time: Option<String>,
    #[serde(default)]
    contains_adult_content: Option<bool>,
    #[serde(default)]
    available: Option<bool>,
    #[serde(default)]
    status: Option<String>,
}

impl ModInfo {
    /// Who to credit, as far as v1 says: the author, linked to the uploader's profile when the
    /// answer has one, the mod's page, and the adult flag. v1 has no field the attribution doc
    /// could confirm for the author's permissions and credits text, so none is kept and the app
    /// points to the mod's page for them.
    #[must_use]
    #[inline]
    pub fn credits(&self, domain: &GameDomain) -> ModCredits {
        let mut credits = ModCredits::fetched_now();
        credits.author.clone_from(&self.author);
        credits.author_url.clone_from(&self.author_page);
        credits.page_url = Some(crate::id::mod_home(domain, self.id));
        credits.is_adult = self.has_adult_content;
        credits.checked(crate::PLATFORM)
    }
}

/// One of `domain`'s lists, as Nexus orders it.
///
/// # Errors
///
/// As a request, [`Error::Malformed`] when the answer is not an array or nothing in it holds up.
#[inline]
pub async fn feed(client: &Client, domain: &GameDomain, which: Feed) -> Result<Vec<ModInfo>> {
    let call = which.call();
    let url = api_url(&format!("/v1/games/{domain}/mods/{}.json", which.path()))
        .ok_or_else(|| Error::InvalidDomain(domain.to_string()))?;
    let body = client.get_api(url, call, MAX_LIST_BYTES).await?;
    let values: Vec<serde_json::Value> = serde_json::from_slice(&body)
        .map_err(|err| Error::malformed(call, format!("line {}, column {}", err.line(), err.column())))?;
    let mut skipped = 0_usize;
    let mut found = Vec::with_capacity(values.len().min(MAX_LIST));
    for value in values.into_iter().take(MAX_LIST) {
        match serde_json::from_value::<RawMod>(value)
            .ok()
            .and_then(|raw| validate(&raw))
        {
            Some(info) => found.push(info),
            None => skipped = skipped.saturating_add(1),
        }
    }
    tracing::info!(%domain, feed = which.path(), mods = found.len(), skipped, "nexus mod list read");
    if found.is_empty() && skipped > 0 {
        return Err(Error::malformed(
            call,
            format!("none of its {skipped} mods could be read"),
        ));
    }
    Ok(found)
}

/// Mod `id` of `domain`.
///
/// # Errors
///
/// As a request ([`Error::NotFound`] for a number the game lacks), [`Error::Malformed`] when the
/// answer is not a mod.
#[inline]
pub async fn mod_info(client: &Client, domain: &GameDomain, id: NexusModId) -> Result<ModInfo> {
    const CALL: &str = "the mod";
    let url = api_url(&format!("/v1/games/{domain}/mods/{id}.json"))
        .ok_or_else(|| Error::InvalidDomain(domain.to_string()))?;
    let body = client.get_api(url, CALL, MAX_MOD_BYTES).await?;
    let raw: RawMod = serde_json::from_slice(&body)
        .map_err(|err| Error::malformed(CALL, format!("line {}, column {}", err.line(), err.column())))?;
    let info = validate(&raw).ok_or_else(|| Error::malformed(CALL, "no mod number or name"))?;
    if info.id != id {
        return Err(Error::malformed(CALL, "it is another mod"));
    }
    Ok(info)
}

fn validate(raw: &RawMod) -> Option<ModInfo> {
    let id = NexusModId::new(raw.mod_id)?;
    let name = raw
        .name
        .as_deref()
        .map(|name| display_text(name, MAX_NAME_CHARS))
        .filter(|name| !name.is_empty())?;
    let field = |given: Option<&str>| {
        given
            .map(|text| display_text(text, MAX_FIELD_CHARS))
            .filter(|shown| !shown.is_empty())
    };
    let is_published = raw.status.as_deref().is_none_or(|status| status == "published");
    Some(ModInfo {
        id,
        name,
        summary: raw
            .summary
            .as_deref()
            .map(|text| display_text(&text.replace("<br />", " "), MAX_SUMMARY_CHARS))
            .unwrap_or_default(),
        picture: raw.picture_url.as_deref().and_then(checked_picture),
        version: field(raw.version.as_deref()),
        author: field(raw.author.as_deref()),
        author_page: raw
            .uploaded_users_profile_url
            .as_deref()
            .map(str::trim)
            .filter(|url| !url.is_empty() && url.len() <= MAX_URL)
            .map(str::to_owned),
        endorsements: raw.endorsement_count.unwrap_or_default(),
        updated: raw.updated_time.as_deref().and_then(date),
        has_adult_content: raw.contains_adult_content.unwrap_or_default(),
        is_available: raw.available.unwrap_or(true) && is_published,
    })
}
