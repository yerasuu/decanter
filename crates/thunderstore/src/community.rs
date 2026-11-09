// SPDX-License-Identifier: GPL-3.0-or-later
//! Thunderstore's communities, one per game, and which of them a library game is.
//!
//! The list comes from `/api/experimental/community/`, known only from secondary sources
//! (analysis, section 2): read as `results` of `{identifier, name}` with a cursor in
//! `pagination.next_link`. It changes rarely, so it is cached for a week inside the library.
//! A game is matched by its title against the communities' names and identifiers; the match is
//! offered, never applied, since only the user knows a remaster from its original.

use core::time::Duration;
use std::collections::HashSet;
use std::fs;
use std::io;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::http::Client;
use crate::id::{CommunityId, DOMAIN, checked_url, display_text};
use crate::{Error, Refresh, Result};

const CALL: &str = "the community list";
const CACHE_FILE: &str = "communities.json";
const TTL: Duration = Duration::from_hours(7 * 24);
const MAX_PAGE_BYTES: u64 = 8 << 20;
const MAX_PAGES: usize = 50;
const MAX_COMMUNITIES: usize = 10_000;
const MAX_NAME_CHARS: usize = 200;
const MAX_SUGGESTIONS: usize = 8;
/// Words too common in titles to say two of them are the same game.
const COMMON_WORDS: [&str; 6] = ["the", "of", "and", "edition", "game", "remastered"];

#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct Community {
    pub id: CommunityId,
    pub name: String,
}

#[derive(Deserialize)]
struct Page {
    #[serde(default)]
    pagination: Option<Pagination>,
    #[serde(default)]
    results: Vec<serde_json::Value>,
}

#[derive(Deserialize)]
struct Pagination {
    #[serde(default)]
    next_link: Option<String>,
}

#[derive(Serialize, Deserialize)]
struct RawCommunity {
    identifier: String,
    #[serde(default)]
    name: Option<String>,
}

#[derive(Serialize, Deserialize)]
struct Cached {
    communities: Vec<RawCommunity>,
}

/// Every community, from the cache while it is fresh. A cache that does not hold up is asked
/// again rather than trusted.
///
/// # Errors
///
/// As a request ([`Error::Network`], [`Error::Unavailable`], ...), [`Error::Malformed`] when no
/// page holds a community decanter can read, or [`Error::Io`] when the cache can't be written.
#[inline]
pub async fn communities(client: &Client, cache: &Path, refresh: Refresh) -> Result<Vec<Community>> {
    let path = cache.join(CACHE_FILE);
    if refresh == Refresh::IfStale
        && let Some(cached) = read_cache(&path)
    {
        return Ok(cached);
    }
    let fetched = fetch(client).await?;
    write_cache(cache, &path, &fetched)?;
    Ok(fetched)
}

async fn fetch(client: &Client) -> Result<Vec<Community>> {
    let mut url = checked_url(&format!("https://{DOMAIN}/api/experimental/community/"))
        .ok_or_else(|| Error::malformed(CALL, "its address does not parse"))?;
    let mut communities = Vec::new();
    let mut seen = HashSet::new();
    let mut skipped = 0_usize;
    let mut visited = HashSet::new();
    for _ in 0..MAX_PAGES {
        visited.insert(url.to_string());
        let body = client.get_bytes(url.clone(), CALL, MAX_PAGE_BYTES).await?;
        let page: Page = serde_json::from_slice(&body)
            .map_err(|err| Error::malformed(CALL, format!("line {}, column {}", err.line(), err.column())))?;
        for value in page.results {
            match serde_json::from_value::<RawCommunity>(value)
                .ok()
                .and_then(|raw| validate(&raw))
            {
                Some(community) if seen.insert(community.id.clone()) => communities.push(community),
                Some(_) => {}
                None => skipped = skipped.saturating_add(1),
            }
            if communities.len() >= MAX_COMMUNITIES {
                break;
            }
        }
        let next = page
            .pagination
            .and_then(|pagination| pagination.next_link)
            .and_then(|link| checked_url(&link))
            .filter(|link| link.path().starts_with("/api/experimental/community/"))
            .filter(|link| !visited.contains(link.as_str()));
        match next {
            Some(link) if communities.len() < MAX_COMMUNITIES => url = link,
            _ => break,
        }
    }
    tracing::info!(
        communities = communities.len(),
        skipped,
        "thunderstore community list read"
    );
    if communities.is_empty() {
        return Err(Error::malformed(
            CALL,
            format!("no community could be read ({skipped} skipped)"),
        ));
    }
    communities.sort_by_key(|community| community.name.to_lowercase());
    Ok(communities)
}

fn validate(raw: &RawCommunity) -> Option<Community> {
    let id = raw.identifier.parse::<CommunityId>().ok()?;
    let name = raw
        .name
        .as_deref()
        .map(|name| display_text(name, MAX_NAME_CHARS))
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| id.as_str().to_owned());
    Some(Community { id, name })
}

fn read_cache(path: &Path) -> Option<Vec<Community>> {
    let metadata = fs::metadata(path).ok()?;
    let age = metadata.modified().ok()?.elapsed().ok()?;
    if age > TTL || metadata.len() > MAX_PAGE_BYTES {
        return None;
    }
    let text = fs::read(path).ok()?;
    match serde_json::from_slice::<Cached>(&text) {
        Ok(cached) => {
            let communities: Vec<Community> = cached.communities.iter().filter_map(validate).collect();
            (!communities.is_empty()).then_some(communities)
        }
        Err(err) => {
            tracing::warn!(path = %path.display(), %err, "thunderstore community cache unreadable, asking again");
            None
        }
    }
}

fn write_cache(cache: &Path, path: &Path, communities: &[Community]) -> Result<()> {
    fs::create_dir_all(cache).map_err(Error::io(cache))?;
    let text = serde_json::to_vec(&Cached {
        communities: communities
            .iter()
            .map(|community| RawCommunity {
                identifier: community.id.as_str().to_owned(),
                name: Some(community.name.clone()),
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

/// Which communities a game titled `title` may be, best first.
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct Suggestions {
    /// Indexes into the list given to [`suggest`].
    pub candidates: Vec<usize>,
    /// Whether the first candidate's name or identifier is the title itself, letters and digits
    /// compared; the user still confirms it.
    pub is_exact: bool,
}

/// The communities whose name or identifier matches `title`: the same letters and digits first,
/// then one holding the other, then those sharing a significant word.
#[must_use]
#[inline]
pub fn suggest(communities: &[Community], title: &str) -> Suggestions {
    let wanted = squashed(title);
    let words = significant_words(title);
    if wanted.is_empty() {
        return Suggestions::default();
    }
    let mut scored: Vec<(u8, usize)> = communities
        .iter()
        .enumerate()
        .filter_map(|(index, community)| {
            let name = squashed(&community.name);
            let id = squashed(community.id.as_str());
            let score = if name == wanted || id == wanted {
                3
            } else if [&name, &id]
                .iter()
                .any(|known| known.len() >= 4 && (known.contains(&wanted) || wanted.contains(known.as_str())))
            {
                2
            } else {
                u8::from(!significant_words(&community.name).is_disjoint(&words))
            };
            (score > 0).then_some((score, index))
        })
        .collect();
    scored.sort_by_key(|&(score, index)| (core::cmp::Reverse(score), index));
    Suggestions {
        is_exact: scored.first().is_some_and(|&(score, _)| score == 3)
            && scored.get(1).is_none_or(|&(score, _)| score < 3),
        candidates: scored
            .into_iter()
            .take(MAX_SUGGESTIONS)
            .map(|(_, index)| index)
            .collect(),
    }
}

fn squashed(text: &str) -> String {
    text.chars()
        .filter(|character| character.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

fn significant_words(text: &str) -> HashSet<String> {
    text.split(|character: char| !character.is_alphanumeric())
        .map(str::to_lowercase)
        .filter(|word| word.chars().count() >= 4 && !COMMON_WORDS.contains(&word.as_str()))
        .collect()
}
