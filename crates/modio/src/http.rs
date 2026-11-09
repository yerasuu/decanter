// SPDX-License-Identifier: GPL-3.0-or-later
//! The HTTP client of every request to mod.io: HTTPS only, a connect and a read time limit, at
//! most five redirects and only to mod.io's hosts over HTTPS, bodies capped, requests paced, and
//! a 429 honoured for every client in the process.
//!
//! The reference gives a user-linked key 60 requests a minute and warns that ignoring 429s can
//! revoke credentials, so decanter keeps to one request every 1.5 s, each started by a user action,
//! and after a 429 sends nothing until its `retry-after` has passed (60 s when it says 0 or
//! nothing, as the reference reads). The key goes in the `api_key` query parameter, so no URL is
//! logged or kept in an error, and it is sent only to the API host, never to a download's.

use core::sync::atomic::{AtomicU64, Ordering};
use core::time::Duration;
use std::sync::{Arc, LazyLock};
use std::time::Instant;

use decanter_core::secrets::Secret;
use reqwest::{Response, StatusCode, redirect};
use serde::Deserialize;
use serde::de::DeserializeOwned;

use crate::error::TERMS_REF;
use crate::id::is_modio_host;
use crate::{Error, Result};

const API_BASE: &str = "https://api.mod.io/v1";
const MAX_REDIRECTS: usize = 5;
const PACE: Duration = Duration::from_millis(1500);
const DEFAULT_RETRY_AFTER: u64 = 60;
/// A `retry-after` past this is taken as this, so that a bad header can't lock mod.io out for days.
const MAX_RETRY_AFTER: u64 = 3600;
const MAX_ERROR_BODY: u64 = 64 << 10;
/// The longest key decanter keeps; mod.io's are 32 hex digits as far as the reference shows.
const MAX_KEY_LEN: usize = 128;
const USER_AGENT: &str = concat!(
    "decanter/",
    env!("CARGO_PKG_VERSION"),
    " (+https://github.com/yerasuu/decanter; read-only mod browser)"
);

/// When the next request may start, in milliseconds since [`EPOCH`], shared by every client so
/// two panels never double the rate.
static NEXT_SLOT: AtomicU64 = AtomicU64::new(0);
/// Until when mod.io's last 429 forbids asking, in milliseconds since [`EPOCH`].
static BLOCKED_UNTIL: AtomicU64 = AtomicU64::new(0);
static EPOCH: LazyLock<Instant> = LazyLock::new(Instant::now);

/// Whether `text` can be a mod.io API key: 1 to 128 ASCII letters and digits. Checked where a key
/// is typed and where it is read back from the keyring.
#[must_use]
#[inline]
pub fn is_key_shaped(text: &str) -> bool {
    (1..=MAX_KEY_LEN).contains(&text.len()) && text.bytes().all(|byte| byte.is_ascii_alphanumeric())
}

/// Cheap to clone: reqwest's client and the key are shared underneath.
#[derive(Debug, Clone)]
pub struct Client {
    http: reqwest::Client,
    key: Arc<Secret>,
}

#[derive(Deserialize)]
struct ErrorBody {
    error: ErrorDetail,
}

#[derive(Deserialize)]
struct ErrorDetail {
    #[serde(default)]
    error_ref: Option<u32>,
}

impl Client {
    /// # Errors
    ///
    /// [`Error::NoKey`] when `key` is not the shape of a key, [`Error::Setup`] when the TLS stack
    /// can't be set up.
    #[inline]
    pub fn new(key: Secret) -> Result<Self> {
        if !is_key_shaped(key.expose()) {
            return Err(Error::NoKey);
        }
        // A read limit rather than a total one, since a file of gigabytes legitimately takes
        // minutes; a stalled answer still ends.
        let http = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(15))
            .read_timeout(Duration::from_secs(60))
            .https_only(true)
            .user_agent(USER_AGENT)
            .redirect(redirect::Policy::custom(|attempt| {
                let stays = attempt.url().scheme() == "https"
                    && attempt.url().port().is_none()
                    && attempt.url().host_str().is_some_and(is_modio_host);
                if attempt.previous().len() > MAX_REDIRECTS {
                    attempt.error("too many redirects")
                } else if stays {
                    attempt.follow()
                } else {
                    attempt.error("redirected off mod.io's hosts")
                }
            }))
            .build()
            .map_err(Error::Setup)?;
        Ok(Self {
            http,
            key: Arc::new(key),
        })
    }

    /// The JSON answer of a GET on the API's `path` (from `/`, built from checked ids only) with
    /// `query` and the key, refused past `limit` bytes.
    pub(crate) async fn api<T>(&self, path: &str, query: &[(&str, &str)], call: &'static str, limit: u64) -> Result<T>
    where
        T: DeserializeOwned,
    {
        let mut url =
            url::Url::parse(&format!("{API_BASE}{path}")).map_err(|_err| Error::malformed(call, "bad address"))?;
        url.query_pairs_mut()
            .extend_pairs(query)
            .append_pair("api_key", self.key.expose());
        let body = body(self.send(url, call).await?, call, limit).await?;
        serde_json::from_slice(&body)
            .map_err(|err| Error::malformed(call, format!("line {}, column {}", err.line(), err.column())))
    }

    /// The 2xx answer of a GET on a download address, which carries no key: the address names
    /// the file itself and may be on another host.
    pub(crate) async fn file(&self, url: url::Url, call: &'static str) -> Result<Response> {
        self.send(url, call).await
    }

    async fn send(&self, url: url::Url, call: &'static str) -> Result<Response> {
        blocked(call)?;
        pace().await;
        blocked(call)?;
        tracing::debug!(call, host = url.host_str().unwrap_or_default(), "modio request");
        let response = self
            .http
            .get(url)
            .send()
            .await
            .map_err(|err| Error::network(call, err))?;
        let status = response.status();
        if status.is_success() {
            return Ok(response);
        }
        if status == StatusCode::TOO_MANY_REQUESTS {
            let seconds = retry_after(&response);
            let error_ref = error_ref(response).await;
            block_for(seconds);
            tracing::warn!(
                call,
                seconds,
                ?error_ref,
                "modio rate limit hit, every request held back"
            );
            return Err(Error::RateLimited { call, seconds });
        }
        if status == StatusCode::NOT_FOUND || status == StatusCode::GONE {
            return Err(Error::NotFound { call });
        }
        let code = status.as_u16();
        let error_ref = error_ref(response).await;
        tracing::warn!(call, status = code, ?error_ref, "modio refused a request");
        match status {
            StatusCode::UNAUTHORIZED => Err(Error::KeyRejected { call }),
            StatusCode::FORBIDDEN if error_ref == Some(TERMS_REF) => Err(Error::TermsRequired { call }),
            StatusCode::FORBIDDEN => Err(Error::Forbidden {
                call,
                error_ref: error_ref.unwrap_or_default(),
            }),
            _ => Err(Error::Unavailable { call, status: code }),
        }
    }
}

/// The `error_ref` of a refusal's JSON body, which says what the status alone does not.
async fn error_ref(response: Response) -> Option<u32> {
    let read = body(response, "an error answer", MAX_ERROR_BODY).await.ok()?;
    serde_json::from_slice::<ErrorBody>(&read).ok()?.error.error_ref
}

async fn body(mut response: Response, call: &'static str, limit: u64) -> Result<Vec<u8>> {
    let too_large = || Error::TooLarge { call, limit };
    let declared = response.content_length();
    if declared.is_some_and(|length| length > limit) {
        return Err(too_large());
    }
    let mut body = Vec::with_capacity(
        declared
            .and_then(|length| usize::try_from(length).ok())
            .unwrap_or_default(),
    );
    while let Some(chunk) = response.chunk().await.map_err(|err| Error::network(call, err))? {
        let grown = u64::try_from(body.len().saturating_add(chunk.len())).unwrap_or(u64::MAX);
        if grown > limit {
            return Err(too_large());
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

/// The wait a 429 asks for, in seconds: 60 when it says 0 or nothing, as the reference reads.
fn retry_after(response: &Response) -> u64 {
    response
        .headers()
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.trim().parse::<u64>().ok())
        .filter(|&seconds| seconds > 0)
        .unwrap_or(DEFAULT_RETRY_AFTER)
        .min(MAX_RETRY_AFTER)
}

fn now_ms() -> u64 {
    u64::try_from(EPOCH.elapsed().as_millis()).unwrap_or(u64::MAX)
}

fn blocked(call: &'static str) -> Result<()> {
    let wait = BLOCKED_UNTIL.load(Ordering::Relaxed).saturating_sub(now_ms());
    if wait == 0 {
        return Ok(());
    }
    Err(Error::RateLimited {
        call,
        seconds: wait.div_ceil(1000),
    })
}

fn block_for(seconds: u64) {
    let until = now_ms().saturating_add(seconds.saturating_mul(1000));
    BLOCKED_UNTIL.fetch_max(until, Ordering::Relaxed);
}

/// Waits for this request's slot: [`PACE`] after the one before it, whichever task asked.
async fn pace() {
    let pace = u64::try_from(PACE.as_millis()).unwrap_or(u64::MAX);
    let now = now_ms();
    let slot = NEXT_SLOT
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |next| {
            Some(next.max(now).saturating_add(pace))
        })
        .map_or(now, |previous| previous.max(now));
    let wait = slot.saturating_sub(now);
    if wait > 0 {
        tokio::time::sleep(Duration::from_millis(wait)).await;
    }
}
