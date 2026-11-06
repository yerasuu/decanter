// SPDX-License-Identifier: GPL-3.0-or-later
//! The account's owned games: the entitlements listing of Amazon's distribution service.
//!
//! The endpoint, headers, body and answer fields are nile's as docs/amazon-nile-integration-analysis.md
//! section 3 reads them. Nothing here has been checked against Amazon (UNVERIFIED until the owner's
//! spike, kb #1519), so the answer is read tolerantly: every field but the ids and the title is
//! optional, one entry that does not fit is skipped and counted, and counts and lengths are capped.

use core::fmt::Write as _;

use reqwest::StatusCode;
use reqwest::header::{HeaderName, HeaderValue};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest as _, Sha256};

use crate::error::CallError;
use crate::http::{self, Call};
use crate::identity::{
    DISTRIBUTION_USER_AGENT, ENTITLEMENTS_CLIENT_ID, ENTITLEMENTS_KEY_ID, ENTITLEMENTS_PATH, ENTITLEMENTS_TARGET,
    GAMING_HOST,
};
use crate::session::{self, Live, LiveSession};

/// A 401 or 403 with a token that was just renewed or is still valid means Amazon closed the
/// session, as for the auth service.
const LISTING: Call = Call {
    name: "library listing",
    refused: &[StatusCode::UNAUTHORIZED, StatusCode::FORBIDDEN],
    max_body: 8 << 20,
};

/// nile asks for 50 entitlements a page (UNVERIFIED that Amazon honours more or fewer).
const PAGE_SIZE: u32 = 50;
/// Past this many pages the listing stops and says it is incomplete, so a service that never
/// stops answering `nextToken` cannot keep decanter asking.
const MAX_PAGES: usize = 200;
const MAX_GAMES: usize = 10_000;
const NEXT_TOKEN_MAX_LEN: usize = 4096;
const ID_MAX_LEN: usize = 128;
const TITLE_MAX_CHARS: usize = 256;
const MAX_GENRES: usize = 16;
const GENRE_MAX_CHARS: usize = 64;
const IMAGE_URL_MAX_LEN: usize = 1024;

/// One owned game, as the entitlement describes it.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct AmazonGame {
    /// `product.id`: what installs, manifests and updates are keyed by (analysis, section 3), so
    /// it is the game's id in decanter.
    pub product_id: String,
    /// The entitlement's own `id`: what the download request of a later stage takes.
    pub entitlement_id: String,
    /// `product.sku`, which a launched game is told as `AMAZON_GAMES_FUEL_PRODUCT_SKU`.
    pub sku: Option<String>,
    pub title: String,
    /// `product.productDetail.details.genres`, as Amazon spells them. Empty when it gives none.
    pub genres: Vec<String>,
    /// `productDetail.iconUrl`.
    pub icon_url: Option<String>,
    /// `details.pgCrownImageUrl`, the tile Amazon's own launcher draws for the game.
    pub cover_url: Option<String>,
    /// `details.backgroundUrl1`, or `backgroundUrl2` when the first is missing.
    pub background_url: Option<String>,
}

/// What a listing found.
#[derive(Debug)]
#[non_exhaustive]
pub struct Listing {
    pub games: Vec<AmazonGame>,
    /// Entries of the answer that were not a game decanter could use (no usable ids or title, a
    /// repeated product, or a shape that does not parse).
    pub skipped: usize,
    /// Whether every page was read. A listing that is not complete must not be taken as the
    /// whole truth about what the account owns.
    pub complete: bool,
}

/// What [`list_games`] found.
#[derive(Debug)]
#[non_exhaustive]
pub enum Library {
    Listed(Listing),
    LoggedOut,
    /// Amazon no longer accepts the account's session: only a new browser login brings it back.
    Refused,
    /// Amazon could not be asked or did not answer in a usable way: nothing is known about the
    /// library, so what is cached should stay.
    Unreachable(String),
}

/// One page of the answer. Entries stay raw so that one of an unexpected shape costs that entry,
/// not the page.
#[derive(Deserialize)]
struct Page {
    #[serde(default)]
    entitlements: Vec<Value>,
    #[serde(default, rename = "nextToken")]
    next_token: Option<String>,
}

#[derive(Deserialize)]
struct Entitlement {
    id: String,
    product: Product,
}

#[derive(Deserialize)]
struct Product {
    id: String,
    #[serde(default)]
    sku: Option<String>,
    title: Option<String>,
    #[serde(default, rename = "productDetail")]
    detail: Option<ProductDetail>,
}

#[derive(Deserialize)]
struct ProductDetail {
    details: Option<Details>,
    #[serde(default, rename = "iconUrl")]
    icon_url: Option<String>,
}

#[derive(Deserialize)]
struct Details {
    #[serde(default)]
    genres: Vec<Value>,
    #[serde(default, rename = "pgCrownImageUrl")]
    crown_image_url: Option<String>,
    #[serde(default, rename = "backgroundUrl1")]
    background_url_1: Option<String>,
    #[serde(default, rename = "backgroundUrl2")]
    background_url_2: Option<String>,
}

#[derive(Serialize)]
struct Request<'req> {
    #[serde(rename = "Operation")]
    operation: &'static str,
    #[serde(rename = "clientId")]
    client_id: &'static str,
    /// nile sends the time of its previous listing to get only what is new; decanter always asks
    /// for everything, since the cache prunes by what a complete listing mentions.
    #[serde(rename = "syncPoint")]
    sync_point: Option<f64>,
    #[serde(rename = "nextToken")]
    next_token: Option<&'req str>,
    #[serde(rename = "maxResults")]
    max_results: u32,
    #[serde(rename = "productIdFilter")]
    product_id_filter: Option<()>,
    #[serde(rename = "keyId")]
    key_id: &'static str,
    #[serde(rename = "hardwareHash")]
    hardware_hash: &'req str,
}

/// Lists the account's games, renewing its session first when needed.
///
/// The art is what `productDetail` carries (kb #1607): an icon, the crown tile as the cover and
/// the first background.
///
/// # Errors
///
/// Only a local problem: the keyring, a stored session that does not parse, the HTTP client.
/// Anything Amazon says or fails to say is a [`Library`].
#[inline]
pub async fn list_games(account_id: &str) -> anyhow::Result<Library> {
    let session = match session::live_session(account_id).await? {
        Live::Ready(session) => session,
        Live::LoggedOut => return Ok(Library::LoggedOut),
        Live::Refused => return Ok(Library::Refused),
        Live::Unreachable(reason) => return Ok(Library::Unreachable(reason)),
    };
    let http = http::client_for(GAMING_HOST, DISTRIBUTION_USER_AGENT)?;
    match read_listing(&http, &session).await {
        Ok(listing) => {
            tracing::info!(
                account_id,
                games = listing.games.len(),
                skipped = listing.skipped,
                complete = listing.complete,
                "listed the amazon library"
            );
            Ok(Library::Listed(listing))
        }
        Err(CallError::Rejected { call, status }) => {
            tracing::info!(account_id, call, status, "amazon refused the session for the library");
            Ok(Library::Refused)
        }
        Err(err @ (CallError::Network { .. } | CallError::Unavailable { .. } | CallError::Malformed { .. })) => {
            let reason = session::reason(err);
            tracing::warn!(account_id, reason, "amazon did not answer the library listing");
            Ok(Library::Unreachable(reason))
        }
        Err(err @ (CallError::Setup(_) | CallError::Random(_))) => Err(anyhow::Error::new(err)),
    }
}

async fn read_listing(http: &reqwest::Client, session: &LiveSession) -> Result<Listing, CallError> {
    let hardware_hash = hardware_hash(&session.device_serial);
    let mut token = HeaderValue::from_str(session.access_token.expose()).map_err(|err| CallError::Malformed {
        call: LISTING.name,
        reason: format!("the access token is not a valid header value: {err}"),
    })?;
    token.set_sensitive(true);
    let mut listing = Listing {
        games: Vec::new(),
        skipped: 0,
        complete: false,
    };
    let mut next_token: Option<String> = None;
    for _ in 0..MAX_PAGES {
        let body = Request {
            operation: "GetEntitlements",
            client_id: ENTITLEMENTS_CLIENT_ID,
            sync_point: None,
            next_token: next_token.as_deref(),
            max_results: PAGE_SIZE,
            product_id_filter: None,
            key_id: ENTITLEMENTS_KEY_ID,
            hardware_hash: &hardware_hash,
        };
        let request = http
            .post(format!("https://{GAMING_HOST}{ENTITLEMENTS_PATH}"))
            .header(HeaderName::from_static("x-amz-target"), ENTITLEMENTS_TARGET)
            .header(HeaderName::from_static("x-amzn-token"), token.clone())
            .header("UserAgent", DISTRIBUTION_USER_AGENT)
            .header("Content-Encoding", "amz-1.0")
            .json(&body);
        let page: Page = http::send(request, &LISTING).await?;
        for raw in page.entitlements {
            match game(raw) {
                Some(found) if listing.games.iter().all(|known| known.product_id != found.product_id) => {
                    if listing.games.len() >= MAX_GAMES {
                        return Ok(listing);
                    }
                    listing.games.push(found);
                }
                Some(_) | None => listing.skipped = listing.skipped.saturating_add(1),
            }
        }
        next_token = match page.next_token.filter(|next| !next.is_empty()) {
            None => {
                listing.complete = true;
                return Ok(listing);
            }
            Some(next) if next.len() > NEXT_TOKEN_MAX_LEN || next_token.as_deref() == Some(next.as_str()) => {
                return Ok(listing);
            }
            Some(next) => Some(next),
        };
    }
    Ok(listing)
}

/// The entry as a game, or `None` when it lacks usable ids or a title.
fn game(raw: Value) -> Option<AmazonGame> {
    let entitlement: Entitlement = serde_json::from_value(raw).ok()?;
    let Product {
        id,
        sku,
        title: raw_title,
        detail,
    } = entitlement.product;
    let title = printable(&raw_title?, TITLE_MAX_CHARS)?;
    let icon_url = detail.as_ref().and_then(|found| image(found.icon_url.as_deref()));
    let details = detail.and_then(|found| found.details);
    let (cover_url, background_url) = details.as_ref().map_or((None, None), |found| {
        (
            image(found.crown_image_url.as_deref()),
            image(found.background_url_1.as_deref()).or_else(|| image(found.background_url_2.as_deref())),
        )
    });
    let genres = details
        .map(|details| {
            details
                .genres
                .iter()
                .filter_map(Value::as_str)
                .filter_map(|genre| printable(genre, GENRE_MAX_CHARS))
                .take(MAX_GENRES)
                .collect()
        })
        .unwrap_or_default();
    (is_id(&id) && is_id(&entitlement.id)).then_some(AmazonGame {
        product_id: id,
        entitlement_id: entitlement.id,
        sku: sku.as_deref().and_then(|sku| printable(sku, ID_MAX_LEN)),
        title,
        genres,
        icon_url,
        cover_url,
        background_url,
    })
}

/// An image address the interface may fetch: HTTPS, bounded, with no control characters.
fn image(url: Option<&str>) -> Option<String> {
    let url = url?.trim();
    (url.len() <= IMAGE_URL_MAX_LEN
        && url.starts_with("https://")
        && !url.chars().any(|c| c.is_control() || c.is_whitespace()))
    .then(|| url.to_owned())
}

/// Ids become a database key and, in later stages, a folder name and a request field.
pub(crate) fn is_id(id: &str) -> bool {
    (1..=ID_MAX_LEN).contains(&id.len())
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_' | b':'))
}

/// Control characters go and the text is cut at `max_chars`; `None` when nothing is left.
fn printable(text: &str, max_chars: usize) -> Option<String> {
    let cleaned: String = text.chars().filter(|c| !c.is_control()).take(max_chars).collect();
    let trimmed = cleaned.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_owned())
}

/// Upper-case hex SHA-256 of the device serial, as nile sends it (analysis, section 3).
fn hardware_hash(serial: &str) -> String {
    Sha256::digest(serial.as_bytes())
        .iter()
        .fold(String::with_capacity(64), |mut hex, byte| {
            let _infallible = write!(hex, "{byte:02X}");
            hex
        })
}
