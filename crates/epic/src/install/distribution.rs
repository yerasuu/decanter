// SPDX-License-Identifier: GPL-3.0-or-later
//! Where a game's current build comes from: the launcher's manifest answer (spec, 5.1), the
//! manifest file it points at on the CDNs (5.2 and 5.3), and the CDN base URLs its chunks are under.
//!
//! The manifest URLs carry signed query parameters, so they never reach a log or an error: only
//! host names do (spec, 13.14).

use core::time::Duration;

use reqwest::header::{RETRY_AFTER, USER_AGENT};
use reqwest::{Method, StatusCode, redirect};
use serde::Deserialize;
use serde_json::Value;
use url::Url;

use super::Target;
use crate::error::CallError;
use crate::hosts::Host;
use crate::http::{self, Call, Kind};
use crate::identity::CDN_USER_AGENT;
use crate::manifest::{MAX_RAW_LEN, Sha1Digest};
use crate::session::{self, Live, Session};

const MANIFEST_ANSWER: Call = Call {
    name: "game manifest",
    host: Host::Launcher,
    kind: Kind::Bearer,
    max_body: 1 << 20,
};

/// A manifest of tens of mebibytes on a slow line: the call is anonymous and resumable, so a long
/// limit costs only the wait.
const MANIFEST_TIMEOUT: Duration = Duration::from_mins(5);
const MAX_REDIRECTS: usize = 5;
const MAX_CDNS: usize = 16;
const MAX_PARAMS: usize = 32;

/// The current build of a game: the manifest's SHA-1, where to fetch it and where its chunks are.
pub(super) struct GameDownload {
    expected: Sha1Digest,
    /// Signed: never logged.
    manifest_urls: Vec<Url>,
    /// Each CDN's base for chunks, without a trailing `/`, in the answer's order.
    pub(super) bases: Vec<String>,
}

impl GameDownload {
    /// The CDN hosts, for logs.
    pub(super) fn hosts(&self) -> Vec<&str> {
        self.manifest_urls.iter().filter_map(Url::host_str).collect()
    }
}

/// What asking for the current build ended in, short of a local failure.
pub(super) enum Asked {
    Ready(GameDownload),
    SessionClosed,
    /// Epic or the network did not answer: the download pauses and is carried on later.
    Unreachable(String),
}

#[derive(Deserialize)]
struct Answer {
    #[serde(default)]
    elements: Option<Vec<Value>>,
}

#[derive(Deserialize)]
struct Element {
    #[serde(default)]
    hash: Option<String>,
    #[serde(default)]
    manifests: Option<Vec<Value>>,
    #[serde(rename = "isPreloaded", default)]
    is_preloaded: Option<bool>,
}

#[derive(Deserialize)]
struct Entry {
    #[serde(default)]
    uri: Option<String>,
    #[serde(rename = "queryParams", default)]
    query_params: Option<Vec<Param>>,
}

#[derive(Deserialize)]
struct Param {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    value: Option<String>,
}

/// Asks the launcher service for `target`'s current build on Windows, label `Live` (spec, 5.1),
/// renewing the session once when Epic refuses its token (spec, 13.4).
///
/// # Errors
///
/// A local failure, a moved route, or an answer of a shape spec 13.9 says to tell apart.
pub(super) async fn game_download(target: &Target<'_>) -> anyhow::Result<Asked> {
    let mut session = match session::live(target.account_id).await? {
        Live::Ready(session) => session,
        Live::Closed => return Ok(Asked::SessionClosed),
        Live::Unreachable(reason) => return Ok(Asked::Unreachable(reason)),
    };
    let client = http::client()?;
    let mut asked = ask(&client, &session, target).await;
    if matches!(asked, Err(CallError::Revoked { .. })) {
        session = match session::renew_refused(&session).await? {
            Live::Ready(fresh) => fresh,
            Live::Closed => return Ok(Asked::SessionClosed),
            Live::Unreachable(reason) => return Ok(Asked::Unreachable(reason)),
        };
        asked = ask(&client, &session, target).await;
    }
    let answer = match asked {
        Ok(answer) => answer,
        Err(CallError::Revoked { .. } | CallError::ActionRequired { .. }) => return Ok(Asked::SessionClosed),
        Err(err @ (CallError::TimedOut { .. } | CallError::Unreachable { .. } | CallError::Unavailable { .. })) => {
            return Ok(Asked::Unreachable(session::reason(err)));
        }
        Err(err) => return Err(session::failed("Epic did not give this game's manifest", err)),
    };
    read_answer(answer).map(Asked::Ready)
}

async fn ask(client: &reqwest::Client, session: &Session, target: &Target<'_>) -> Result<Answer, CallError> {
    let path = format!(
        "/launcher/api/public/assets/v2/platform/Windows/namespace/{}/catalogItem/{}/app/{}/label/Live",
        target.namespace, target.catalog_item_id, target.app_name
    );
    let request =
        http::request(client, Method::GET, &MANIFEST_ANSWER, &path).bearer_auth(session.access_token().expose());
    http::send(request, &MANIFEST_ANSWER).await
}

/// The one element spec 5.1 allows, its hash and its usable CDN entries.
fn read_answer(answer: Answer) -> anyhow::Result<GameDownload> {
    let mut elements = answer.elements.unwrap_or_default();
    anyhow::ensure!(
        !elements.is_empty(),
        "Epic's manifest answer for this game has no build"
    );
    anyhow::ensure!(
        elements.len() == 1,
        "Epic's manifest answer for this game has {} builds, and which one to install is unknown",
        elements.len()
    );
    let element: Element = serde_json::from_value(elements.swap_remove(0))
        .map_err(|_| anyhow::anyhow!("Epic's manifest answer for this game is not the expected shape"))?;
    let expected = element
        .hash
        .as_deref()
        .and_then(sha1_hex)
        .ok_or_else(|| anyhow::anyhow!("Epic's manifest answer for this game has no usable hash"))?;
    if element.is_preloaded == Some(true) {
        tracing::info!("epic build is a preload; its manifest is likely encrypted");
    }
    let entries = element
        .manifests
        .ok_or_else(|| anyhow::anyhow!("Epic's manifest answer for this game names no CDN"))?;
    let listed = entries.len();
    let mut manifest_urls = Vec::new();
    let mut bases = Vec::new();
    let mut unusable = 0_usize;
    for entry in entries.into_iter().take(MAX_CDNS) {
        match cdn(entry) {
            Some((manifest_url, base)) => {
                if !bases.contains(&base) {
                    bases.push(base);
                }
                manifest_urls.push(manifest_url);
            }
            None => unusable = unusable.saturating_add(1),
        }
    }
    tracing::info!(
        listed,
        usable = manifest_urls.len(),
        unusable,
        bases = bases.len(),
        hosts = ?manifest_urls.iter().filter_map(Url::host_str).collect::<Vec<_>>(),
        "epic manifest answer read"
    );
    anyhow::ensure!(
        !manifest_urls.is_empty(),
        "none of the {listed} CDNs Epic named for this game is usable over HTTPS"
    );
    Ok(GameDownload {
        expected,
        manifest_urls,
        bases,
    })
}

/// One CDN entry: its manifest URL, `uri` plus its query parameters as given (spec, 5.2), and its
/// chunk base, `uri` without its last segment. Plain HTTP is refused (spec, 5.3 step 5).
fn cdn(entry: Value) -> Option<(Url, String)> {
    let entry: Entry = serde_json::from_value(entry).ok()?;
    let uri = Url::parse(entry.uri.as_deref()?).ok()?;
    if uri.scheme() != "https" || uri.host_str().is_none() || uri.fragment().is_some() {
        return None;
    }
    let params = entry.query_params.unwrap_or_default();
    if params.len() > MAX_PARAMS || (uri.query().is_some() && !params.is_empty()) {
        return None;
    }
    let mut pairs = Vec::with_capacity(params.len());
    for param in params {
        pairs.push(format!("{}={}", param.name?, param.value?));
    }
    let manifest_url = if pairs.is_empty() {
        uri.clone()
    } else {
        Url::parse(&format!("{uri}?{}", pairs.join("&"))).ok()?
    };
    let mut bare = uri;
    bare.set_query(None);
    let text = bare.as_str();
    let base = text.get(..text.rfind('/')?)?.to_owned();
    Some((manifest_url, base))
}

fn sha1_hex(text: &str) -> Option<Sha1Digest> {
    if text.len() != 40 || !text.is_ascii() {
        return None;
    }
    let mut out = [0_u8; 20];
    for (index, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(text.get(index * 2..index * 2 + 2)?, 16).ok()?;
    }
    Some(Sha1Digest(out))
}

/// The anonymous client of the manifest and chunk downloads (spec, 2.3): the launcher's CDN user
/// agent, no `Authorization`, HTTPS only, and redirects held to the host asked or Epic's own
/// (spec, 13.12).
pub(crate) fn cdn_client() -> Result<reqwest::Client, CallError> {
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(USER_AGENT, reqwest::header::HeaderValue::from_static(CDN_USER_AGENT));
    reqwest::Client::builder()
        .default_headers(headers)
        .connect_timeout(Duration::from_secs(10))
        .read_timeout(Duration::from_secs(30))
        .https_only(true)
        .redirect(redirect::Policy::custom(|attempt| {
            let asked = attempt.previous().first().and_then(|url| url.host_str());
            let target = attempt.url().host_str();
            let stays = attempt.url().scheme() == "https"
                && (target == asked || target.is_some_and(|host| host.ends_with(".epicgames.com")));
            if attempt.previous().len() > MAX_REDIRECTS {
                attempt.error("too many redirects")
            } else if stays {
                attempt.follow()
            } else {
                attempt.error("redirected off the CDN asked")
            }
        }))
        .build()
        .map_err(CallError::Setup)
}

/// Why no manifest came from any CDN.
pub(super) enum ManifestFetch {
    /// No CDN answered with the file: the download pauses.
    Unreachable(String),
    /// A CDN answered with bytes whose SHA-1 is not the answer's, and none with the right ones.
    Mismatch,
    TooLarge,
}

/// The manifest file from the first CDN that gives it whole and with the answer's SHA-1, trying
/// the next on any failure (spec, 5.3 and 13.9: a CDN's error page with a 200 is not parsed).
pub(super) async fn manifest(download: &GameDownload) -> Result<Vec<u8>, ManifestFetch> {
    let client = cdn_client().map_err(|err| ManifestFetch::Unreachable(session::reason(err)))?;
    let (mut failed, mut mismatched, mut too_large) = (0_usize, 0_usize, 0_usize);
    let mut last = String::new();
    for url in &download.manifest_urls {
        let host = url.host_str().unwrap_or_default();
        match fetch_capped(&client, url, MAX_RAW_LEN, MANIFEST_TIMEOUT).await {
            Ok(body) if Sha1Digest::of(&body) == download.expected => {
                tracing::info!(host, bytes = body.len(), "epic manifest fetched");
                return Ok(body);
            }
            Ok(body) => {
                mismatched = mismatched.saturating_add(1);
                tracing::warn!(
                    host,
                    bytes = body.len(),
                    "epic manifest from this CDN has the wrong SHA-1"
                );
            }
            Err(Fetched::TooLarge) => too_large = too_large.saturating_add(1),
            Err(err) => {
                failed = failed.saturating_add(1);
                last = format!("{host}: {err}");
                tracing::warn!(host, error = %err, "epic manifest not fetched from this CDN");
            }
        }
    }
    tracing::warn!(failed, mismatched, too_large, "no CDN gave the epic manifest");
    if mismatched > 0 {
        Err(ManifestFetch::Mismatch)
    } else if too_large > 0 {
        Err(ManifestFetch::TooLarge)
    } else {
        Err(ManifestFetch::Unreachable(last))
    }
}

/// Why one anonymous download failed.
pub(crate) enum Fetched {
    /// 401, 403, 404 or 410: this CDN will not give the file.
    Refused(u16),
    /// A 429, with how long it asked to wait.
    RateLimited(Option<Duration>),
    Network(String),
    TooLarge,
}

impl core::fmt::Display for Fetched {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Refused(status) => write!(f, "refused (HTTP {status})"),
            Self::RateLimited(_) => f.write_str("rate limited (HTTP 429)"),
            Self::Network(reason) => f.write_str(reason),
            Self::TooLarge => f.write_str("larger than allowed"),
        }
    }
}

/// GET `url` and its body of at most `limit` bytes; only a 200 is an answer (spec, 7.4).
pub(crate) async fn fetch_capped(
    client: &reqwest::Client,
    url: &Url,
    limit: usize,
    timeout: Duration,
) -> Result<Vec<u8>, Fetched> {
    let network = |err: reqwest::Error| Fetched::Network(err.without_url().to_string());
    let mut response = client.get(url.clone()).timeout(timeout).send().await.map_err(network)?;
    let status = response.status();
    match status {
        StatusCode::OK => {}
        StatusCode::TOO_MANY_REQUESTS => {
            let wait = response
                .headers()
                .get(RETRY_AFTER)
                .and_then(|value| value.to_str().ok())
                .and_then(|value| value.trim().parse::<u64>().ok())
                .map(Duration::from_secs);
            return Err(Fetched::RateLimited(wait));
        }
        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN | StatusCode::NOT_FOUND | StatusCode::GONE => {
            return Err(Fetched::Refused(status.as_u16()));
        }
        other => return Err(Fetched::Network(format!("HTTP {}", other.as_u16()))),
    }
    let declared = response
        .content_length()
        .and_then(|length| usize::try_from(length).ok());
    if declared.is_some_and(|length| length > limit) {
        return Err(Fetched::TooLarge);
    }
    let mut body = Vec::with_capacity(declared.unwrap_or_default().min(limit));
    while let Some(chunk) = response.chunk().await.map_err(network)? {
        if body.len().saturating_add(chunk.len()) > limit {
            return Err(Fetched::TooLarge);
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}
