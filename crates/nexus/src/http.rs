// SPDX-License-Identifier: GPL-3.0-or-later
//! The HTTP clients of every request to Nexus Mods: HTTPS only, a connect and a read time limit,
//! at most five redirects, bodies capped, and the `Application-Name` and `Application-Version`
//! headers the acceptable use policy requires.
//!
//! Two clients, because the key travels in an `apikey` header that reqwest does not strip on a
//! redirect the way it strips `Authorization`: the API client follows redirects only on the API
//! host and is the only one that carries the key; the download client follows them only on Nexus'
//! and its CDN's hosts and never carries it, since a download link is already signed.
//!
//! Nexus documents 20 000 requests a day and then 500 an hour per user (analysis, section 1); the
//! `X-RL-*-Remaining` headers it counts them in are secondary. Every answer's counts are kept, and
//! once both are out, or on a 429, no request leaves until the next full hour, when the hourly
//! allowance resets. API requests are also paced to one a second, each one started by a user action.

use core::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use core::time::Duration;
use std::sync::{Arc, LazyLock};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use decanter_core::secrets::Secret;
use reqwest::header::{ACCEPT, HeaderMap, HeaderValue};
use reqwest::{Response, StatusCode, redirect};

use crate::id::{API_HOST, is_download_host};
use crate::{Error, Result};

const MAX_REDIRECTS: usize = 5;
const PACE: Duration = Duration::from_secs(1);
const HOUR_SECS: u64 = 3600;
/// Stable across versions, as the acceptable use policy asks of the application name.
const APPLICATION_NAME: &str = "decanter";
const APPLICATION_VERSION: &str = env!("CARGO_PKG_VERSION");
const USER_AGENT: &str = concat!(
    "decanter/",
    env!("CARGO_PKG_VERSION"),
    " (+https://github.com/yerasuu/decanter; personal API key)"
);
fn application_headers() -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert("Application-Name", HeaderValue::from_static(APPLICATION_NAME));
    headers.insert("Application-Version", HeaderValue::from_static(APPLICATION_VERSION));
    headers
}

/// No count seen yet.
const UNKNOWN: u64 = u64::MAX;

static NEXT_SLOT: AtomicU64 = AtomicU64::new(0);
static EPOCH: LazyLock<Instant> = LazyLock::new(Instant::now);
static DAILY_REMAINING: AtomicU64 = AtomicU64::new(UNKNOWN);
static HOURLY_REMAINING: AtomicU64 = AtomicU64::new(UNKNOWN);
/// Seconds since the Unix epoch before which no API request is sent.
static BLOCKED_UNTIL: AtomicU64 = AtomicU64::new(0);
static REFUSED_LOCALLY: AtomicU32 = AtomicU32::new(0);

/// The last counts Nexus gave of this account's requests left, `None` before any answer had them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct Remaining {
    pub daily: Option<u64>,
    pub hourly: Option<u64>,
}

#[must_use]
#[inline]
pub fn remaining() -> Remaining {
    let known = |count: u64| (count != UNKNOWN).then_some(count);
    Remaining {
        daily: known(DAILY_REMAINING.load(Ordering::Relaxed)),
        hourly: known(HOURLY_REMAINING.load(Ordering::Relaxed)),
    }
}

/// Cheap to clone: reqwest's clients and the key are shared underneath.
#[derive(Debug, Clone)]
pub struct Client {
    api: reqwest::Client,
    download: reqwest::Client,
    key: Arc<Secret>,
}

impl Client {
    /// A client that asks with `key`, the user's personal API key.
    ///
    /// # Errors
    ///
    /// [`Error::NoKey`] for an empty key, [`Error::Setup`] when the TLS stack can't be set up.
    #[inline]
    pub fn new(key: Secret) -> Result<Self> {
        if key.expose().trim().is_empty() {
            return Err(Error::NoKey);
        }
        let mut headers = application_headers();
        headers.insert(ACCEPT, HeaderValue::from_static("application/json"));
        let api = builder()
            .default_headers(headers)
            .redirect(policy(
                |host| host.eq_ignore_ascii_case(API_HOST),
                "redirected off the API host",
            ))
            .build()
            .map_err(Error::Setup)?;
        // Downloads name decanter too, since the policy counts blank request metadata against an
        // application wherever its requests go.
        let download = builder()
            .default_headers(application_headers())
            .redirect(policy(is_download_host, "redirected off Nexus' download hosts"))
            .build()
            .map_err(Error::Setup)?;
        Ok(Self {
            api,
            download,
            key: Arc::new(key),
        })
    }

    /// The 2xx body of an API GET for `url`, refused once it grows past `limit` bytes.
    pub(crate) async fn get_api(&self, url: url::Url, call: &'static str, limit: u64) -> Result<Vec<u8>> {
        check_allowance()?;
        pace().await;
        let mut key = HeaderValue::from_str(self.key.expose().trim()).map_err(|_err| Error::KeyRefused)?;
        key.set_sensitive(true);
        tracing::debug!(call, "nexus api request");
        let response = self
            .api
            .get(url)
            .header("apikey", key)
            .send()
            .await
            .map_err(|err| Error::network(call, err))?;
        count_allowance(response.headers(), call);
        let mut answer = checked(response, call)?;
        let too_large = || Error::TooLarge { call, limit };
        let declared = answer.content_length();
        if declared.is_some_and(|length| length > limit) {
            return Err(too_large());
        }
        let mut body = Vec::with_capacity(
            declared
                .and_then(|length| usize::try_from(length).ok())
                .unwrap_or_default(),
        );
        while let Some(chunk) = answer.chunk().await.map_err(|err| Error::network(call, err))? {
            let grown = u64::try_from(body.len().saturating_add(chunk.len())).unwrap_or(u64::MAX);
            if grown > limit {
                return Err(too_large());
            }
            body.extend_from_slice(&chunk);
        }
        Ok(body)
    }

    /// The 2xx answer to a GET of a signed download link, without the key.
    pub(crate) async fn get_download(&self, url: url::Url, call: &'static str) -> Result<Response> {
        tracing::debug!(
            call,
            host = url.host_str().unwrap_or_default(),
            "nexus download request"
        );
        let response = self
            .download
            .get(url)
            .send()
            .await
            .map_err(|err| Error::network(call, err))?;
        checked(response, call)
    }
}

fn builder() -> reqwest::ClientBuilder {
    // A read limit rather than a total one, since a mod of gigabytes legitimately takes minutes;
    // a stalled answer still ends.
    reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(15))
        .read_timeout(Duration::from_secs(60))
        .https_only(true)
        .user_agent(USER_AGENT)
}

fn policy(is_allowed: fn(&str) -> bool, refusal: &'static str) -> redirect::Policy {
    redirect::Policy::custom(move |attempt| {
        let stays = attempt.url().scheme() == "https"
            && attempt.url().port().is_none()
            && attempt.url().host_str().is_some_and(is_allowed);
        if attempt.previous().len() > MAX_REDIRECTS {
            attempt.error("too many redirects")
        } else if stays {
            attempt.follow()
        } else {
            attempt.error(refusal)
        }
    })
}

fn checked(response: Response, call: &'static str) -> Result<Response> {
    let status = response.status();
    if status.is_success() {
        return Ok(response);
    }
    tracing::warn!(call, status = status.as_u16(), "nexus refused a request");
    Err(match status {
        StatusCode::UNAUTHORIZED => Error::KeyRefused,
        StatusCode::NOT_FOUND | StatusCode::GONE => Error::NotFound { call },
        StatusCode::TOO_MANY_REQUESTS => {
            block_until_next_hour();
            Error::RateLimited { resets: resets_text() }
        }
        _ => Error::Unavailable {
            call,
            status: status.as_u16(),
        },
    })
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_secs())
}

fn block_until_next_hour() {
    let next = now_secs()
        .checked_div(HOUR_SECS)
        .unwrap_or_default()
        .saturating_add(1)
        .saturating_mul(HOUR_SECS);
    BLOCKED_UNTIL.fetch_max(next, Ordering::Relaxed);
    tracing::warn!(
        blocked_until_unix = next,
        "nexus request limit used up, no request until the next hour"
    );
}

/// When requests may go again, as `HH:00 UTC`.
fn resets_text() -> String {
    let until = BLOCKED_UNTIL.load(Ordering::Relaxed);
    let hour = until
        .checked_div(HOUR_SECS)
        .unwrap_or_default()
        .checked_rem(24)
        .unwrap_or_default();
    format!("{hour:02}:00 UTC")
}

fn check_allowance() -> Result<()> {
    if now_secs() < BLOCKED_UNTIL.load(Ordering::Relaxed) {
        let refused = REFUSED_LOCALLY.fetch_add(1, Ordering::Relaxed).saturating_add(1);
        tracing::warn!(refused, "nexus request not sent, the request limit is used up");
        return Err(Error::RateLimited { resets: resets_text() });
    }
    Ok(())
}

fn count_allowance(headers: &HeaderMap, call: &'static str) {
    let count = |name: &str| headers.get(name)?.to_str().ok()?.trim().parse::<u64>().ok();
    let daily = count("x-rl-daily-remaining");
    let hourly = count("x-rl-hourly-remaining");
    if let Some(left) = daily {
        DAILY_REMAINING.store(left, Ordering::Relaxed);
    }
    if let Some(left) = hourly {
        HOURLY_REMAINING.store(left, Ordering::Relaxed);
    }
    if daily.is_none() && hourly.is_none() {
        tracing::debug!(call, "nexus answer without rate-limit headers");
    }
    if daily == Some(0) && hourly == Some(0) {
        block_until_next_hour();
    }
}

/// Waits for this request's slot: [`PACE`] after the one before it, whichever task asked.
async fn pace() {
    let pace = u64::try_from(PACE.as_millis()).unwrap_or(u64::MAX);
    let now = u64::try_from(EPOCH.elapsed().as_millis()).unwrap_or(u64::MAX);
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
