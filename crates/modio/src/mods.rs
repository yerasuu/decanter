// SPDX-License-Identifier: GPL-3.0-or-later
//! A game's mods: a page of them searched and sorted by mod.io (`GET /games/{id}/mods`), one mod
//! (`/mods/{id}`), its files (`/files`) and the mods it depends on (`/dependencies`).
//!
//! The dependencies endpoint exists, but the analysis could not read its answer's shape
//! (section 3): each entry is read for a `mod_id` and a `name` and nothing more, an entry without
//! them is skipped and counted, and a failure leaves the detail without the list rather than
//! failing it. Dependencies are shown, never installed on their own: what installing them means is
//! not documented.

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

#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct RemoteMod {
    pub game: GameId,
    pub id: ModioModId,
    pub name: String,
    pub summary: String,
    pub author: Option<String>,
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
}

#[derive(Deserialize)]
struct RawUser {
    #[serde(default)]
    username: Option<String>,
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
fn validate(raw: RawMod, game: GameId) -> Option<RemoteMod> {
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
        author: raw
            .submitted_by
            .and_then(|user| user.username)
            .map(|text| display_text(&text, MAX_NAME_CHARS))
            .filter(|text| !text.is_empty()),
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
    })
}

/// A page of `game`'s mods, from `offset`, matching `query` when it is not empty, most downloaded
/// first.
///
/// # Errors
///
/// As a request, or [`Error::Malformed`] when the answer is not a page of mods.
#[inline]
pub async fn search_mods(client: &Client, game: GameId, query: &str, offset: u64) -> Result<ModsPage> {
    let wanted: String = query.trim().chars().take(MAX_QUERY_CHARS).collect();
    let limit = PAGE_SIZE.to_string();
    let offset_text = offset.to_string();
    let mut params = vec![
        ("_limit", limit.as_str()),
        ("_offset", offset_text.as_str()),
        ("_sort", "-downloads_total"),
    ];
    if !wanted.is_empty() {
        params.push(("_q", wanted.as_str()));
    }
    let page: RawPage = client
        .api(&format!("/games/{game}/mods"), &params, SEARCH_CALL, MAX_PAGE_BYTES)
        .await?;
    let mut skipped = 0_usize;
    let mut mods = Vec::with_capacity(page.data.len());
    for value in page.data {
        match serde_json::from_value::<RawMod>(value)
            .ok()
            .and_then(|raw| validate(raw, game))
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
    let found = validate(raw, game).ok_or_else(|| Error::malformed(MOD_CALL, "no id or name"))?;
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
