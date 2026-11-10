// SPDX-License-Identifier: GPL-3.0-or-later
//! Nexus' games, one domain each, and which of them a library game is.
//!
//! The list is `/v1/games.json` (secondary, analysis section 1), read as an array of
//! `{id, name, domain_name}`. It changes rarely and every request counts against the user's limit,
//! so it is cached for a week inside the library. A game is matched by its title against the games'
//! names and domains; the match is offered, never applied, since only the user knows a remaster
//! from its original.

use core::time::Duration;
use std::collections::HashSet;
use std::fs;
use std::io;
use std::path::Path;

use decanter_mods::Likeness;
use serde::{Deserialize, Serialize};

use crate::http::Client;
use crate::id::{GameDomain, api_url, display_text};
use crate::{Error, Refresh, Result};

const CALL: &str = "the game list";
const CACHE_FILE: &str = "games.json";
const TTL: Duration = Duration::from_hours(7 * 24);
const MAX_BYTES: u64 = 32 << 20;
const MAX_GAMES: usize = 50_000;
const MAX_NAME_CHARS: usize = 200;
const MAX_SUGGESTIONS: usize = 8;
/// Words too common in titles to say two of them are the same game.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct Game {
    pub domain: GameDomain,
    pub name: String,
    /// How many mods Nexus counts for it, to tell two of the same name apart.
    pub mods: Option<u64>,
}

#[derive(Serialize, Deserialize)]
struct RawGame {
    domain_name: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    mods: Option<u64>,
}

#[derive(Serialize, Deserialize)]
struct Cached {
    games: Vec<RawGame>,
}

/// Every game, from the cache while it is fresh. A cache that does not hold up is asked again
/// rather than trusted.
///
/// # Errors
///
/// As a request, [`Error::Malformed`] when no game in the answer can be read, or [`Error::Io`]
/// when the cache can't be written.
#[inline]
pub async fn games(client: &Client, cache: &Path, refresh: Refresh) -> Result<Vec<Game>> {
    let path = cache.join(CACHE_FILE);
    if refresh == Refresh::IfStale
        && let Some(cached) = read_cache(&path)
    {
        return Ok(cached);
    }
    let url = api_url("/v1/games.json").ok_or_else(|| Error::malformed(CALL, "its address does not parse"))?;
    let body = client.get_api(url, CALL, MAX_BYTES).await?;
    let values: Vec<serde_json::Value> = serde_json::from_slice(&body)
        .map_err(|err| Error::malformed(CALL, format!("line {}, column {}", err.line(), err.column())))?;
    let mut seen = HashSet::new();
    let mut skipped = 0_usize;
    let mut games = Vec::with_capacity(values.len().min(MAX_GAMES));
    for value in values.into_iter().take(MAX_GAMES) {
        match serde_json::from_value::<RawGame>(value)
            .ok()
            .and_then(|raw| validate(&raw))
        {
            Some(game) if seen.insert(game.domain.clone()) => games.push(game),
            Some(_) => {}
            None => skipped = skipped.saturating_add(1),
        }
    }
    tracing::info!(games = games.len(), skipped, "nexus game list read");
    if games.is_empty() {
        return Err(Error::malformed(
            CALL,
            format!("no game could be read ({skipped} skipped)"),
        ));
    }
    games.sort_by_key(|game| game.name.to_lowercase());
    write_cache(cache, &path, &games)?;
    Ok(games)
}

fn validate(raw: &RawGame) -> Option<Game> {
    let domain = raw.domain_name.parse::<GameDomain>().ok()?;
    let name = raw
        .name
        .as_deref()
        .map(|name| display_text(name, MAX_NAME_CHARS))
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| domain.as_str().to_owned());
    Some(Game {
        domain,
        name,
        mods: raw.mods,
    })
}

fn read_cache(path: &Path) -> Option<Vec<Game>> {
    let metadata = fs::metadata(path).ok()?;
    let age = metadata.modified().ok()?.elapsed().ok()?;
    if age > TTL || metadata.len() > MAX_BYTES {
        return None;
    }
    let text = fs::read(path).ok()?;
    match serde_json::from_slice::<Cached>(&text) {
        Ok(cached) => {
            let games: Vec<Game> = cached.games.iter().filter_map(validate).collect();
            (!games.is_empty()).then_some(games)
        }
        Err(err) => {
            tracing::warn!(path = %path.display(), %err, "nexus game cache unreadable, asking again");
            None
        }
    }
}

fn write_cache(cache: &Path, path: &Path, games: &[Game]) -> Result<()> {
    fs::create_dir_all(cache).map_err(Error::io(cache))?;
    let text = serde_json::to_vec(&Cached {
        games: games
            .iter()
            .map(|game| RawGame {
                domain_name: game.domain.as_str().to_owned(),
                name: Some(game.name.clone()),
                mods: game.mods,
            })
            .collect(),
    })
    .map_err(|err| Error::Settings {
        path: path.to_path_buf(),
        reason: err.to_string(),
    })?;
    let partial = path.with_extension(format!("json.{}.tmp", std::process::id()));
    fs::write(&partial, text).map_err(Error::io(&partial))?;
    fs::rename(&partial, path).map_err(|err| {
        if let Err(cleanup) = fs::remove_file(&partial)
            && cleanup.kind() != io::ErrorKind::NotFound
        {
            tracing::warn!(path = %partial.display(), %cleanup, "could not remove a partial cache file");
        }
        Error::io(path)(err)
    })
}

/// Which games a library game titled `title` may be, best first.
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct Suggestions {
    /// Indexes into the list given to [`suggest`].
    pub candidates: Vec<usize>,
    /// Whether the first candidate's name or domain is the title itself, letters and digits
    /// compared; the user still confirms it.
    pub is_exact: bool,
}

/// The games whose name or domain plausibly is `title` ([`decanter_mods::likeness`]).
///
/// The same letters and digits first, then one holding the other, then those sharing significant
/// words; more mods first among equals. A game sharing only a loose word is left out.
#[must_use]
#[inline]
pub fn suggest(games: &[Game], title: &str) -> Suggestions {
    let mut scored: Vec<(Likeness, u64, usize)> = games
        .iter()
        .enumerate()
        .filter_map(|(index, game)| {
            let found = decanter_mods::best_likeness(title, [game.name.as_str(), game.domain.as_str()])?;
            Some((found, game.mods.unwrap_or_default(), index))
        })
        .collect();
    scored.sort_by_key(|&(found, mods, index)| (core::cmp::Reverse(found), core::cmp::Reverse(mods), index));
    Suggestions {
        is_exact: scored.first().is_some_and(|&(found, _, _)| found == Likeness::Exact)
            && scored.get(1).is_none_or(|&(found, _, _)| found < Likeness::Exact),
        candidates: scored
            .into_iter()
            .take(MAX_SUGGESTIONS)
            .map(|(_, _, index)| index)
            .collect(),
    }
}
