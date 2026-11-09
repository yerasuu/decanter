// SPDX-License-Identifier: GPL-3.0-or-later
//! The HTTP client of every request to Thunderstore: HTTPS only, a connect and a read time limit,
//! at most five redirects and only to Thunderstore's hosts over HTTPS, bodies capped, and requests
//! paced. Thunderstore publishes no rate limit, so decanter keeps to one request a second, each
//! one started by a user action, and names itself in the User-Agent so it can be told apart.

use core::sync::atomic::{AtomicU64, Ordering};
use core::time::Duration;
use std::sync::LazyLock;
use std::time::Instant;

use reqwest::{Response, StatusCode, redirect};

use crate::id::is_thunderstore_host;
use crate::{Error, Result};

const MAX_REDIRECTS: usize = 5;
const PACE: Duration = Duration::from_secs(1);
const USER_AGENT: &str = concat!(
    "decanter/",
    env!("CARGO_PKG_VERSION"),
    " (+https://github.com/yerasuu/decanter; read-only mod browser)"
);

/// When the next request may start, in milliseconds since [`EPOCH`], shared by every client so
/// two panels never double the rate.
static NEXT_SLOT: AtomicU64 = AtomicU64::new(0);
static EPOCH: LazyLock<Instant> = LazyLock::new(Instant::now);

/// Cheap to clone: reqwest's client is shared underneath.
#[derive(Debug, Clone)]
pub struct Client {
    http: reqwest::Client,
}

impl Client {
    /// # Errors
    ///
    /// [`Error::Setup`] when the TLS stack can't be set up.
    #[inline]
    pub fn new() -> Result<Self> {
        // A read limit rather than a total one, since a large community's listing or a package
        // of gigabytes legitimately takes minutes; a stalled answer still ends.
        let http = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(15))
            .read_timeout(Duration::from_secs(60))
            .https_only(true)
            .user_agent(USER_AGENT)
            .redirect(redirect::Policy::custom(|attempt| {
                let stays = attempt.url().scheme() == "https"
                    && attempt.url().port().is_none()
                    && attempt.url().host_str().is_some_and(is_thunderstore_host);
                if attempt.previous().len() > MAX_REDIRECTS {
                    attempt.error("too many redirects")
                } else if stays {
                    attempt.follow()
                } else {
                    attempt.error("redirected off Thunderstore's hosts")
                }
            }))
            .build()
            .map_err(Error::Setup)?;
        Ok(Self { http })
    }

    /// Sends a GET for `url` once its pace slot comes, and returns the 2xx answer.
    pub(crate) async fn get(&self, url: url::Url, call: &'static str) -> Result<Response> {
        pace().await;
        tracing::debug!(call, host = url.host_str().unwrap_or_default(), "thunderstore request");
        let response = self
            .http
            .get(url)
            .send()
            .await
            .map_err(|err| Error::network(call, err))?;
        let status = response.status();
        if status == StatusCode::NOT_FOUND || status == StatusCode::GONE {
            return Err(Error::NotFound { call });
        }
        if !status.is_success() {
            tracing::warn!(call, status = status.as_u16(), "thunderstore refused a request");
            return Err(Error::Unavailable {
                call,
                status: status.as_u16(),
            });
        }
        Ok(response)
    }

    /// The 2xx body of a GET for `url`, refused once it grows past `limit` bytes.
    pub(crate) async fn get_bytes(&self, url: url::Url, call: &'static str, limit: u64) -> Result<Vec<u8>> {
        let mut response = self.get(url, call).await?;
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
