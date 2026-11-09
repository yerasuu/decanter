// SPDX-License-Identifier: GPL-3.0-or-later
//! mod.io's games and which of them a library game is.
//!
//! `GET /games?_q=` searches by name; the match is offered, never applied, since only the user
//! knows a remaster from its original. The chosen game's own record (`GET /games/{id}`) is read
//! again whenever its mods are about to be, because a game can switch third-party access off at
//! any time (analysis, section 3) and its `api_access_options` decide whether decanter may go on.

use serde::Deserialize;

use crate::http::Client;
use crate::id::{GameId, checked_url, display_text};
use crate::{Error, Result};

const SEARCH_CALL: &str = "the game search";
const GAME_CALL: &str = "the game's record";
const MAX_PAGE_BYTES: u64 = 4 << 20;
const SEARCH_LIMIT: &str = "20";
const MAX_NAME_CHARS: usize = 200;
const MAX_SUMMARY_CHARS: usize = 500;
/// mod.io's limit on `_q` is not documented where the analysis read; a title never needs more.
const MAX_QUERY_CHARS: usize = 100;

/// The game's `api_access_options` bitfield, read as the reference documents it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Access(u32);

impl Access {
    const THIRD_PARTY: u32 = 1;
    const DIRECT_DOWNLOADS: u32 = 2;
    const TOKEN_FOR_DOWNLOADS: u32 = 4;
    const OWNERSHIP_FOR_DOWNLOADS: u32 = 8;

    /// Bit 1: "Allow 3rd parties to access this game's API endpoints".
    #[must_use]
    #[inline]
    pub const fn allows_third_party(self) -> bool {
        self.0 & Self::THIRD_PARTY != 0
    }

    /// Bit 2: download addresses are stable; without it they carry a hash that changes often.
    #[must_use]
    #[inline]
    pub const fn has_direct_downloads(self) -> bool {
        self.0 & Self::DIRECT_DOWNLOADS != 0
    }

    /// Bits 4 and 8: a download needs a user's bearer token (and with 8, owning the file), which
    /// an API key alone is not.
    #[must_use]
    #[inline]
    pub const fn needs_login(self) -> bool {
        self.0 & (Self::TOKEN_FOR_DOWNLOADS | Self::OWNERSHIP_FOR_DOWNLOADS) != 0
    }
}

#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct Game {
    pub id: GameId,
    pub name: String,
    pub summary: String,
    /// HTTPS on a mod.io host.
    pub logo: Option<String>,
    pub page: Option<String>,
    pub access: Access,
}

#[derive(Deserialize)]
pub(crate) struct RawPage {
    #[serde(default)]
    pub data: Vec<serde_json::Value>,
    #[serde(default)]
    pub result_total: Option<u64>,
}

#[derive(Deserialize)]
pub(crate) struct RawLogo {
    #[serde(default)]
    thumb_320x180: Option<String>,
    #[serde(default)]
    thumb_640x360: Option<String>,
    #[serde(default)]
    original: Option<String>,
}

impl RawLogo {
    /// The smallest thumbnail that holds up, since the browser shows them at 40 to 64 points.
    pub(crate) fn url(&self) -> Option<String> {
        [&self.thumb_320x180, &self.thumb_640x360, &self.original]
            .into_iter()
            .find_map(|text| text.as_deref().and_then(checked_url))
            .map(String::from)
    }
}

#[derive(Deserialize)]
struct RawGame {
    id: u64,
    name: String,
    #[serde(default)]
    summary: Option<String>,
    #[serde(default)]
    logo: Option<RawLogo>,
    #[serde(default)]
    profile_url: Option<String>,
    /// Missing reads as 0: nothing allowed, so a record without it is refused.
    #[serde(default)]
    api_access_options: u32,
}

fn validate(raw: &RawGame) -> Option<Game> {
    let name = display_text(&raw.name, MAX_NAME_CHARS);
    if name.is_empty() {
        return None;
    }
    Some(Game {
        id: GameId::new(raw.id)?,
        name,
        summary: raw
            .summary
            .as_deref()
            .map(|text| display_text(text, MAX_SUMMARY_CHARS))
            .unwrap_or_default(),
        logo: raw.logo.as_ref().and_then(RawLogo::url),
        page: raw.profile_url.as_deref().and_then(checked_url).map(String::from),
        access: Access(raw.api_access_options),
    })
}

/// The games whose name matches `query`, as mod.io orders them, with their access flags so the
/// picker can say which ones decanter can use.
///
/// # Errors
///
/// As a request ([`Error::KeyRejected`], [`Error::RateLimited`], ...), or [`Error::Malformed`]
/// when the answer is not a page of games.
#[inline]
pub async fn search_games(client: &Client, query: &str) -> Result<Vec<Game>> {
    let wanted: String = query.trim().chars().take(MAX_QUERY_CHARS).collect();
    let page: RawPage = client
        .api(
            "/games",
            &[("_q", wanted.as_str()), ("_limit", SEARCH_LIMIT)],
            SEARCH_CALL,
            MAX_PAGE_BYTES,
        )
        .await?;
    let mut skipped = 0_usize;
    let mut games = Vec::with_capacity(page.data.len());
    for value in page.data {
        match serde_json::from_value::<RawGame>(value)
            .ok()
            .as_ref()
            .and_then(validate)
        {
            Some(game) => games.push(game),
            None => skipped = skipped.saturating_add(1),
        }
    }
    tracing::info!(
        found = games.len(),
        skipped,
        total = ?page.result_total,
        "modio game search read"
    );
    Ok(games)
}

/// The game's own record, read fresh.
///
/// # Errors
///
/// As a request, [`Error::NotFound`] for a game mod.io no longer has, or [`Error::Malformed`].
#[inline]
pub async fn game(client: &Client, id: GameId) -> Result<Game> {
    let raw: RawGame = client
        .api(&format!("/games/{id}"), &[], GAME_CALL, MAX_PAGE_BYTES)
        .await?;
    let game = validate(&raw).ok_or_else(|| Error::malformed(GAME_CALL, "no id or name"))?;
    if game.id != id {
        return Err(Error::malformed(GAME_CALL, "another game's record"));
    }
    tracing::info!(
        game_id = %id,
        third_party = game.access.allows_third_party(),
        direct_downloads = game.access.has_direct_downloads(),
        needs_login = game.access.needs_login(),
        "modio game record read"
    );
    Ok(game)
}

/// Refuses a game whose record does not let a third-party tool in. Its downloads may still need a
/// logged-in user, which [`crate::download`] refuses on its own.
///
/// # Errors
///
/// [`Error::NoThirdPartyAccess`].
#[inline]
pub const fn check_access(game: &Game) -> Result<()> {
    if game.access.allows_third_party() {
        Ok(())
    } else {
        Err(Error::NoThirdPartyAccess)
    }
}

/// Asks for one game, the lightest call there is, to tell whether mod.io takes the key.
///
/// # Errors
///
/// [`Error::KeyRejected`] for a key mod.io does not know, or as any request.
#[inline]
pub async fn probe(client: &Client) -> Result<()> {
    let page: RawPage = client
        .api("/games", &[("_limit", "1")], "the key check", MAX_PAGE_BYTES)
        .await?;
    tracing::info!(total = ?page.result_total, "modio key checked");
    Ok(())
}
