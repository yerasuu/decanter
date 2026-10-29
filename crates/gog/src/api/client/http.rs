// SPDX-License-Identifier: GPL-3.0-or-later
//! The one HTTP client and the one session every async call shares. Derived from `decanter-gog`'s
//! `session.rs` (status codes read apart, refresh and retry) and its `http_client`.

use core::time::Duration;

use reqwest::{redirect, RequestBuilder, StatusCode};
use serde::de::DeserializeOwned;
use tokio::sync::RwLock;

use super::auth::{self, Token};
use super::Error;

/// Every host this client talks to, so the redirect policy and the URLs come from one list.
#[derive(Clone, Copy)]
pub(super) enum Host {
    Auth,
    Embed,
}

impl Host {
    const ALL: [Self; 2] = [Self::Auth, Self::Embed];

    const fn name(self) -> &'static str {
        match self {
            Self::Auth => "auth.gog.com",
            Self::Embed => "embed.gog.com",
        }
    }

    pub(super) fn url(self, path: &str) -> String {
        format!("https://{}{path}", self.name())
    }
}

/// What an authorized call answers when GOG turns the access token down.
pub(super) const SESSION_REFUSED: &[StatusCode] =
    &[StatusCode::UNAUTHORIZED, StatusCode::FORBIDDEN];

/// One kind of request: its name in errors, the statuses that mean GOG refused the credentials,
/// the largest body it may answer with, and whether that body holds credentials, which a parse
/// error could quote.
pub(super) struct Call {
    pub name: &'static str,
    pub refused: &'static [StatusCode],
    pub max_body: usize,
    pub has_secrets: bool,
}

/// An async GOG client holding one account's session.
///
/// GOG rotates the refresh token on use, so a renewed [`Token`] must be stored again: read it
/// back with [`Client::token`] after a call and persist it when it changed, failed calls
/// included, since the refresh may have succeeded before the call that then failed.
pub struct Client {
    http: reqwest::Client,
    /// Behind one lock so that callers who find the token expired at the same time refresh it
    /// once: a second refresh would spend a refresh token GOG has already rotated, and GOG reads
    /// that as a revoked session.
    token: RwLock<Token>,
}

impl Client {
    /// A client for a session stored earlier.
    ///
    /// # Errors
    ///
    /// [`Error::Setup`] when the HTTP client cannot be built.
    #[inline]
    pub fn new(token: Token) -> Result<Self, Error> {
        Ok(Self {
            http: http_client()?,
            token: RwLock::new(token),
        })
    }

    /// Trades the one-shot `code` from GOG's login redirect for a new session.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidInput`] for a code that cannot be one, [`Error::Rejected`] when GOG
    /// refuses it (codes are single use and short lived: start the login again), and the other
    /// variants when GOG cannot be asked or answers something unreadable.
    #[inline]
    pub async fn from_login_code(code: &str) -> Result<Self, Error> {
        let http = http_client()?;
        let token = auth::exchange_code(&http, code).await?;
        Ok(Self {
            http,
            token: RwLock::new(token),
        })
    }

    /// The session as it is now, to be stored in place of the one this client started with.
    #[inline]
    pub async fn token(&self) -> Token {
        self.token.read().await.clone()
    }

    /// Renews the access token now, whether or not it has expired; a renewal another caller
    /// makes meanwhile counts as this one.
    ///
    /// # Errors
    ///
    /// [`Error::Rejected`] when GOG refuses the refresh token; the others when GOG cannot be
    /// asked or answers something unreadable.
    #[inline]
    pub async fn refresh(&self) -> Result<Token, Error> {
        let seen = self.token.read().await.access_token.clone();
        self.renew(&seen).await?;
        Ok(self.token().await)
    }

    /// An access token that has not expired, renewed first when it has; `true` when it was
    /// renewed during this call, by this caller or another.
    async fn access_token(&self) -> Result<(String, bool), Error> {
        let seen = {
            let token = self.token.read().await;
            if !token.is_expired() {
                return Ok((token.access_token.clone(), false));
            }
            token.access_token.clone()
        };
        Ok((self.renew(&seen).await?, true))
    }

    /// Replaces the session whose access token was `seen`, unless another caller already did
    /// while this one waited for the lock.
    async fn renew(&self, seen: &str) -> Result<String, Error> {
        let mut token = self.token.write().await;
        if token.access_token == seen {
            *token = auth::refresh(&self.http, &token.refresh_token).await?;
        }
        Ok(token.access_token.clone())
    }

    /// Sends an authorized GET and hands the body to `accept`. A rejection with an access token
    /// GOG revoked before its expiry is worth one refresh and one more try, unless that token
    /// was itself just renewed.
    pub(super) async fn get<T, U>(
        &self,
        call: &Call,
        host: Host,
        path: &str,
        query: &[(&str, &str)],
        accept: impl Fn(T) -> Result<U, Error>,
    ) -> Result<U, Error>
    where
        T: DeserializeOwned,
    {
        let (access_token, renewed) = self.access_token().await?;
        let request = |bearer: &str| {
            self.http
                .get(host.url(path))
                .query(query)
                .bearer_auth(bearer)
                .header("CSRF", "csrf=true")
        };
        match send(request(&access_token), call).await.and_then(&accept) {
            Err(Error::Rejected { .. }) if !renewed => {
                let fresh = self.renew(&access_token).await?;
                send(request(&fresh), call).await.and_then(accept)
            }
            answer => answer,
        }
    }
}

/// reqwest's own default has no timeout, so an answer GOG stalls on would hang the caller
/// forever; every call here is a small JSON round trip, so one flat timeout covers the whole
/// request. Redirects stay on GOG's hosts over HTTPS, since the bearer token rides along.
fn http_client() -> Result<reqwest::Client, Error> {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .https_only(true)
        .redirect(redirect::Policy::custom(|attempt| {
            let is_gog = attempt.url().scheme() == "https"
                && attempt
                    .url()
                    .host_str()
                    .is_some_and(|host| Host::ALL.iter().any(|known| known.name() == host));
            if attempt.previous().len() >= 5 {
                attempt.error("too many redirects")
            } else if is_gog {
                attempt.follow()
            } else {
                attempt.error("redirected off GOG")
            }
        }))
        .build()
        .map_err(Error::Setup)
}

/// Sends `request` and reads a 2xx body of at most `call.max_body` bytes as `T`.
pub(super) async fn send<T>(request: RequestBuilder, call: &Call) -> Result<T, Error>
where
    T: DeserializeOwned,
{
    let mut response = request
        .send()
        .await
        .map_err(|err| Error::network(call.name, err))?;
    let status = response.status();
    if call.refused.contains(&status) {
        return Err(Error::Rejected {
            call: call.name,
            reason: format!("HTTP {}", status.as_u16()),
        });
    }
    if !status.is_success() {
        return Err(Error::Unavailable {
            call: call.name,
            status: status.as_u16(),
        });
    }
    let too_large = || Error::Malformed {
        call: call.name,
        reason: format!("body larger than {} bytes", call.max_body),
    };
    let declared = response
        .content_length()
        .and_then(|length| usize::try_from(length).ok());
    if declared.is_some_and(|length| length > call.max_body) {
        return Err(too_large());
    }
    let mut body = Vec::with_capacity(declared.unwrap_or_default());
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|err| Error::network(call.name, err))?
    {
        if body.len().saturating_add(chunk.len()) > call.max_body {
            return Err(too_large());
        }
        body.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&body).map_err(|err| Error::Malformed {
        call: call.name,
        reason: if call.has_secrets {
            format!(
                "not the expected JSON at line {}, column {}",
                err.line(),
                err.column()
            )
        } else {
            err.to_string()
        },
    })
}
