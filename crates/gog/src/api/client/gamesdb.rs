// SPDX-License-Identifier: GPL-3.0-or-later
//! GOG's GamesDB API: public, unauthenticated, and the same catalog data Heroic uses. Not gog
//! 0.5.0's `get_game_details`, which hits `embed.gog.com/account/...` and rate limits
//! hard under the one-call-per-owned-game pattern a full library listing needs.

use serde::Deserialize;

use super::Error;
use super::http::{Call, Host, Sessionless};

const RELEASE: Call = Call {
    name: "GamesDB release",
    refused: &[],
    max_body: 4 << 20,
    has_secrets: false,
};

/// A GOG release, keyed by the same numeric product id the owned games list returns.
#[derive(Deserialize)]
#[non_exhaustive]
pub struct Release {
    pub title: Localized,
    pub summary: Option<Localized>,
    pub icon: Option<Image>,
    pub game: GameDetails,
}

#[derive(Deserialize)]
#[non_exhaustive]
pub struct GameDetails {
    pub cover: Option<Image>,
    pub vertical_cover: Option<Image>,
    pub background: Option<Image>,
    #[serde(default)]
    pub developers: Vec<Company>,
    #[serde(default)]
    pub publishers: Vec<Company>,
    #[serde(default)]
    pub genres: Vec<Genre>,
    pub first_release_date: Option<String>,
    pub aggregated_rating: Option<f64>,
}

#[derive(Deserialize)]
#[non_exhaustive]
pub struct Company {
    pub name: String,
}

#[derive(Deserialize)]
#[non_exhaustive]
pub struct Genre {
    pub name: Localized,
}

/// GamesDB text fields are keyed by locale, with `"*"` as the language-agnostic fallback.
#[derive(Deserialize)]
pub struct Localized {
    #[serde(rename = "*")]
    default: Option<String>,
    #[serde(rename = "en-US")]
    en_us: Option<String>,
}

impl Localized {
    #[inline]
    #[must_use]
    pub fn resolve(&self) -> Option<String> {
        self.en_us.clone().or_else(|| self.default.clone())
    }
}

/// A sized-image reference. `url_format` carries `{formatter}` (a size/crop variant selector) and
/// `{ext}` placeholders; substituting them with nothing and `jpg` yields a valid default-size
/// image URL.
#[derive(Deserialize)]
pub struct Image {
    url_format: String,
}

impl Image {
    /// GamesDB's `url_format` is normally already a fully-qualified `https://` URL, but only
    /// prepend a scheme when it's actually missing: the webview serves the app under its own
    /// internal scheme, not `https:`, so a bare `//host/...` URL (the convention every other GOG
    /// endpoint uses) never resolves to a real request and the image just shows as broken.
    #[inline]
    #[must_use]
    pub fn resolve(&self) -> String {
        let resolved = self.url_format.replace("{formatter}", "").replace("{ext}", "jpg");
        match resolved.strip_prefix("//") {
            Some(base) => format!("https://{base}"),
            None => resolved,
        }
    }
}

/// Parses GamesDB's `first_release_date` (e.g. `"2012-04-17T00:00:00+0000"`) into unix seconds;
/// `None` on any format surprise rather than failing the whole game, since this is decorative
/// metadata.
#[inline]
#[must_use]
pub fn parse_release_date(date: &str) -> Option<i64> {
    chrono::DateTime::parse_from_str(date, "%Y-%m-%dT%H:%M:%S%z")
        .ok()
        .map(|parsed| parsed.timestamp())
}

impl Sessionless {
    /// One release's catalog data by GOG product id.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidInput`] for an id that is not positive, [`Error::Unavailable`] for a
    /// product GamesDB does not know (a 404), and the others when it cannot be asked or answers
    /// something unreadable.
    #[inline]
    pub async fn gamesdb_release(&self, id: i64) -> Result<Release, Error> {
        if id <= 0 {
            return Err(Error::InvalidInput { what: "product id" });
        }
        self.get(
            &RELEASE,
            Host::GamesDb,
            &format!("/platforms/gog/external_releases/{id}"),
            &[],
            None,
        )
        .await
    }
}
