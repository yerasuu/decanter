// SPDX-License-Identifier: GPL-3.0-or-later
//! A game's mods: a page of them searched and sorted by mod.io (`GET /games/{id}/mods`), one mod
//! (`/mods/{id}`), its files (`/files`) and the mods it depends on (`/dependencies`).
//!
//! The search uses the filters the REST reference documents for that endpoint (`_q`, `tags`,
//! `maturity_option`, `_sort` with `-` for descending, `_limit`, `_offset`); the sort fields are the
//! ones the design read from the reference's filtering notes (design, section 2.1).
//!
//! The dependencies endpoint exists, but the analysis could not read its answer's shape
//! (section 3): each entry is read for a `mod_id` and a `name` and nothing more, an entry without
//! them is skipped and counted, and a failure leaves the detail without the list rather than
//! failing it. Dependencies are shown, never installed on their own: what installing them means is
//! not documented.
//!
//! `credit_options` and `maturity_option` are bit masks; the attribution doc read that bit 1 shows
//! the credits section and that the rest are redistribution, porting, patching and repackaging
//! permissions, without their values. The values below are mod.io's mod object reference as
//! recalled, unverified: each decoded bit only names a permission next to a link to the mod's
//! page, and an unknown bit is logged and left out rather than guessed.

use decanter_mods::ModCredits;
use serde::Deserialize;

use crate::files::{ModFile, RawFile};
use crate::games::{RawLogo, RawPage};
use crate::http::Client;
use crate::id::{GameId, ModioModId, checked_url, date, display_text};
use crate::{Error, Result};

const SEARCH_CALL: &str = "the mod list";
const MOD_CALL: &str = "the mod";
const FILES_CALL: &str = "the mod's files";
const DEPENDENCIES_CALL: &str = "the mod's dependencies";
const MAX_PAGE_BYTES: u64 = 8 << 20;
/// Mods a page shows; the reference caps `_limit` at 100.
pub const PAGE_SIZE: u64 = 30;
const SHOWN_FILES: &str = "10";
const MAX_DEPENDENCIES: &str = "100";
const MAX_NAME_CHARS: usize = 200;
const MAX_SUMMARY_CHARS: usize = 500;
const MAX_QUERY_CHARS: usize = 100;
const MAX_TAG_CHARS: usize = 100;
/// Bit and token of each `credit_options` permission, the token named by the app.
const CREDIT_OPTIONS: [(u32, &str); 11] = [
    (1, "credits"),
    (2, "original"),
    (4, "redistribute-credit"),
    (8, "port-credit"),
    (16, "patch"),
    (32, "patch-credit"),
    (64, "patch-permission"),
    (128, "repackage"),
    (256, "repackage-credit"),
    (512, "repackage-permission"),
    (1024, "resell"),
];

#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct RemoteMod {
    pub game: GameId,
    pub id: ModioModId,
    pub name: String,
    pub summary: String,
    pub author: Option<String>,
    /// The author's profile, HTTPS on a mod.io host.
    pub author_page: Option<String>,
    /// HTTPS on a mod.io host.
    pub logo: Option<String>,
    pub page: Option<String>,
    pub downloads: u64,
    pub subscribers: u64,
    /// mod.io's own wording of the rating ("Very Positive"), shown as it is.
    pub rating: Option<String>,
    pub rating_percent: Option<u32>,
    /// `YYYY-MM-DD`.
    pub added: Option<String>,
    pub updated: Option<String>,
    /// The file mod.io offers now, the one an install takes.
    pub current: Option<ModFile>,
    /// `credit_options` as mod.io gives it.
    pub credit_options: u32,
    /// `maturity_option`: alcohol, drugs, violence, explicit; 0 when none applies.
    pub maturity: u32,
}

impl RemoteMod {
    /// Who to credit and on what terms: `submitted_by` linked to its profile, the mod's page, the
    /// decoded `credit_options` and whether any maturity option is set. Unknown bits are logged at
    /// debug level only, since the browser's detail asks for this every frame.
    #[must_use]
    #[inline]
    pub fn credits(&self) -> ModCredits {
        let mut credits = ModCredits::fetched_now();
        credits.author.clone_from(&self.author);
        credits.author_url.clone_from(&self.author_page);
        credits.page_url.clone_from(&self.page);
        credits.permissions = CREDIT_OPTIONS
            .iter()
            .filter(|&&(bit, _)| self.credit_options & bit != 0)
            .map(|&(_, token)| token.to_owned())
            .collect();
        let unknown = CREDIT_OPTIONS
            .iter()
            .fold(self.credit_options, |left, &(bit, _)| left & !bit);
        if unknown != 0 {
            tracing::debug!(mod_id = %self.id, unknown, "modio credit_options bits not known, left out");
        }
        credits.is_adult = self.maturity != 0;
        credits.checked(crate::PLATFORM)
    }
}

/// What a search of a game's mods asks for.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct ModsQuery {
    pub text: String,
    pub sort: ModsSort,
    /// Tag names of the game's [`crate::TagGroup`]s; a mod must carry them.
    pub tags: Vec<String>,
    /// Leaves out mods with any `maturity_option` set.
    pub hide_mature: bool,
}

impl ModsQuery {
    /// No words, most downloaded first, any tag, mature mods hidden.
    #[must_use]
    #[inline]
    pub fn new() -> Self {
        Self {
            hide_mature: true,
            ..Self::default()
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum ModsSort {
    #[default]
    Downloads,
    Updated,
    Newest,
    Rating,
    Name,
}

impl ModsSort {
    const fn param(self) -> &'static str {
        match self {
            Self::Downloads => "-downloads_total",
            Self::Updated => "-date_updated",
            Self::Newest => "-date_live",
            Self::Rating => "-ratings_weighted_aggregate",
            Self::Name => "name",
        }
    }
}

#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct ModsPage {
    pub mods: Vec<RemoteMod>,
    /// How many match in all, when mod.io says.
    pub total: Option<u64>,
    pub offset: u64,
}

#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct Dependency {
    pub id: ModioModId,
    pub name: String,
}

#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct Detail {
    /// Newest first.
    pub files: Vec<ModFile>,
    /// `None` when the list could not be read; the reason is in the log.
    pub dependencies: Option<Vec<Dependency>>,
}

#[derive(Deserialize)]
struct RawMod {
    id: u64,
    #[serde(default)]
    game_id: u64,
    name: String,
    #[serde(default)]
    summary: Option<String>,
    #[serde(default)]
    submitted_by: Option<RawUser>,
    #[serde(default)]
    logo: Option<RawLogo>,
    #[serde(default)]
    profile_url: Option<String>,
    #[serde(default)]
    date_added: i64,
    #[serde(default)]
    date_updated: i64,
    #[serde(default)]
    modfile: Option<RawFile>,
    #[serde(default)]
    stats: Option<RawStats>,
    #[serde(default)]
    credit_options: u32,
    #[serde(default)]
    maturity_option: u32,
}

#[derive(Deserialize)]
struct RawUser {
    #[serde(default)]
    username: Option<String>,
    #[serde(default)]
    display_name_portal: Option<String>,
    #[serde(default)]
    profile_url: Option<String>,
}

#[derive(Deserialize)]
struct RawStats {
    #[serde(default)]
    downloads_total: u64,
    #[serde(default)]
    subscribers_total: u64,
    #[serde(default)]
    ratings_display_text: Option<String>,
    #[serde(default)]
    ratings_percentage_positive: Option<u32>,
}

#[derive(Deserialize)]
struct RawDependency {
    mod_id: u64,
    #[serde(default)]
    name: Option<String>,
}

/// A mod whose id and name hold up and that belongs to `game`; a game id the answer leaves out is
/// taken as the one asked for.
fn validate(raw: &RawMod, game: GameId) -> Option<RemoteMod> {
    if raw.game_id != 0 && raw.game_id != game.get() {
        return None;
    }
    let name = display_text(&raw.name, MAX_NAME_CHARS);
    if name.is_empty() {
        return None;
    }
    let counts = raw.stats.as_ref();
    Some(RemoteMod {
        game,
        id: ModioModId::new(raw.id)?,
        name,
        summary: raw
            .summary
            .as_deref()
            .map(|text| display_text(text, MAX_SUMMARY_CHARS))
            .unwrap_or_default(),
        author: raw.submitted_by.as_ref().and_then(|user| {
            [user.display_name_portal.as_deref(), user.username.as_deref()]
                .into_iter()
                .flatten()
                .map(|text| display_text(text, MAX_NAME_CHARS))
                .find(|text| !text.is_empty())
        }),
        author_page: raw
            .submitted_by
            .as_ref()
            .and_then(|user| user.profile_url.as_deref())
            .and_then(checked_url)
            .map(String::from),
        logo: raw.logo.as_ref().and_then(RawLogo::url),
        page: raw.profile_url.as_deref().and_then(checked_url).map(String::from),
        downloads: counts.map(|stats| stats.downloads_total).unwrap_or_default(),
        subscribers: counts.map(|stats| stats.subscribers_total).unwrap_or_default(),
        rating: counts
            .and_then(|stats| stats.ratings_display_text.as_deref())
            .map(|text| display_text(text, 40))
            .filter(|text| !text.is_empty()),
        rating_percent: counts
            .and_then(|stats| stats.ratings_percentage_positive)
            .filter(|&percent| percent <= 100),
        added: date(raw.date_added),
        updated: date(raw.date_updated),
        current: raw.modfile.as_ref().and_then(RawFile::validate),
        credit_options: raw.credit_options,
        maturity: raw.maturity_option,
    })
}

/// A page of `game`'s mods, from `offset`, matching `query`'s words when it has any, its tags and
/// maturity, in its order.
///
/// # Errors
///
/// As a request, or [`Error::Malformed`] when the answer is not a page of mods.
#[inline]
pub async fn search_mods(client: &Client, game: GameId, query: &ModsQuery, offset: u64) -> Result<ModsPage> {
    let wanted: String = query.text.trim().chars().take(MAX_QUERY_CHARS).collect();
    // mod.io reads a comma as the separator of the values `tags` must all match.
    let tags = query
        .tags
        .iter()
        .map(|tag| {
            tag.chars()
                .filter(|&character| character != ',')
                .take(MAX_TAG_CHARS)
                .collect::<String>()
        })
        .filter(|tag| !tag.trim().is_empty())
        .collect::<Vec<_>>()
        .join(",");
    let limit = PAGE_SIZE.to_string();
    let offset_text = offset.to_string();
    let mut params = vec![
        ("_limit", limit.as_str()),
        ("_offset", offset_text.as_str()),
        ("_sort", query.sort.param()),
    ];
    if !wanted.is_empty() {
        params.push(("_q", wanted.as_str()));
    }
    if !tags.is_empty() {
        params.push(("tags", tags.as_str()));
    }
    if query.hide_mature {
        params.push(("maturity_option", "0"));
    }
    let page: RawPage = client
        .api(&format!("/games/{game}/mods"), &params, SEARCH_CALL, MAX_PAGE_BYTES)
        .await?;
    let mut skipped = 0_usize;
    let mut mods = Vec::with_capacity(page.data.len());
    for value in page.data {
        match serde_json::from_value::<RawMod>(value)
            .ok()
            .and_then(|raw| validate(&raw, game))
        {
            Some(found) => mods.push(found),
            None => skipped = skipped.saturating_add(1),
        }
    }
    tracing::info!(
        game_id = %game,
        offset,
        found = mods.len(),
        skipped,
        total = ?page.result_total,
        "modio mod page read"
    );
    Ok(ModsPage {
        mods,
        total: page.result_total,
        offset,
    })
}

/// One mod, read fresh: its current file is what an update check compares against.
///
/// # Errors
///
/// As a request, [`Error::NotFound`] for a mod mod.io no longer has, or [`Error::Malformed`].
#[inline]
pub async fn fetch_mod(client: &Client, game: GameId, id: ModioModId) -> Result<RemoteMod> {
    let raw: RawMod = client
        .api(&format!("/games/{game}/mods/{id}"), &[], MOD_CALL, MAX_PAGE_BYTES)
        .await?;
    let found = validate(&raw, game).ok_or_else(|| Error::malformed(MOD_CALL, "no id or name"))?;
    if found.id != id {
        return Err(Error::malformed(MOD_CALL, "another mod's record"));
    }
    Ok(found)
}

/// The mod's latest files and the mods it depends on.
///
/// # Errors
///
/// As a request for the files; the dependencies' failure only leaves them out.
#[inline]
pub async fn detail(client: &Client, game: GameId, id: ModioModId) -> Result<Detail> {
    let page: RawPage = client
        .api(
            &format!("/games/{game}/mods/{id}/files"),
            &[("_sort", "-date_added"), ("_limit", SHOWN_FILES)],
            FILES_CALL,
            MAX_PAGE_BYTES,
        )
        .await?;
    let mut skipped_files = 0_usize;
    let mut files = Vec::with_capacity(page.data.len());
    for value in page.data {
        match serde_json::from_value::<RawFile>(value)
            .ok()
            .and_then(|raw| raw.validate())
        {
            Some(file) => files.push(file),
            None => skipped_files = skipped_files.saturating_add(1),
        }
    }
    files.sort_by_key(|file| core::cmp::Reverse(file.date_added));
    let dependencies = match dependencies(client, game, id).await {
        Ok(found) => Some(found),
        Err(err) => {
            tracing::warn!(game_id = %game, mod_id = %id, %err, "modio dependencies not read");
            None
        }
    };
    tracing::info!(
        game_id = %game,
        mod_id = %id,
        files = files.len(),
        skipped_files,
        dependencies = ?dependencies.as_ref().map(Vec::len),
        "modio mod detail read"
    );
    Ok(Detail { files, dependencies })
}

async fn dependencies(client: &Client, game: GameId, id: ModioModId) -> Result<Vec<Dependency>> {
    let page: RawPage = client
        .api(
            &format!("/games/{game}/mods/{id}/dependencies"),
            &[("_limit", MAX_DEPENDENCIES)],
            DEPENDENCIES_CALL,
            MAX_PAGE_BYTES,
        )
        .await?;
    let mut skipped = 0_usize;
    let mut found = Vec::with_capacity(page.data.len());
    for value in page.data {
        let read = serde_json::from_value::<RawDependency>(value).ok().and_then(|raw| {
            let dependency = ModioModId::new(raw.mod_id)?;
            let name = raw
                .name
                .as_deref()
                .map(|text| display_text(text, MAX_NAME_CHARS))
                .filter(|text| !text.is_empty())
                .unwrap_or_else(|| dependency.to_string());
            Some(Dependency { id: dependency, name })
        });
        match read {
            Some(dependency) => found.push(dependency),
            None => skipped = skipped.saturating_add(1),
        }
    }
    if skipped > 0 {
        tracing::warn!(game_id = %game, mod_id = %id, skipped, "modio dependencies skipped");
    }
    Ok(found)
}
