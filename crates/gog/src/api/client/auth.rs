// SPDX-License-Identifier: GPL-3.0-only
// Based on gog 0.5.0 (gogapi-rs) by nicohman: the token's fields and the `auth.gog.com/token`
// requests come from its `token.rs`; the refused statuses from decanter-gog's `session.rs`.
//! The GOG session and the two requests that create one.

use std::time::{SystemTime, UNIX_EPOCH};

use reqwest::StatusCode;
use serde::{Deserialize, Serialize};

use super::http::{self, Call, Host};
use super::Error;

/// GOG Galaxy's public client id, used by every third-party GOG client since GOG has no
/// per-application registration for this flow.
pub const CLIENT_ID: &str = "46899977096215655";

/// Paired with [`CLIENT_ID`] and just as public (it ships inside every Galaxy install), but it
/// still stays out of logs and errors along with the URLs that carry it.
pub const CLIENT_SECRET: &str = "9d85c43b1482497dbbce61f6e4aa173a433796eeae2ca8c5f6129f2dc4de46d9";

/// Where GOG's login page redirects with the one-shot `code`.
pub const REDIRECT_URI: &str = "https://embed.gog.com/on_login_success?origin=client";

const TOKEN_PATH: &str = "/token";

/// GOG answers a spent or revoked refresh token, or a used login code, with a 400.
const TOKEN_REFUSED: &[StatusCode] = &[
    StatusCode::BAD_REQUEST,
    StatusCode::UNAUTHORIZED,
    StatusCode::FORBIDDEN,
];

const EXCHANGE: Call = Call {
    name: "login code exchange",
    refused: TOKEN_REFUSED,
    max_body: 64 << 10,
    has_secrets: true,
};

const REFRESH: Call = Call {
    name: "token refresh",
    refused: TOKEN_REFUSED,
    max_body: 64 << 10,
    has_secrets: true,
};

/// The longest code accepted; GOG's are well under a hundred characters.
const MAX_CODE_LEN: usize = 2048;

/// A session as `auth.gog.com/token` returns it, plus when it was received.
///
/// Its JSON is what decanter stores in the OS keyring, so field names, order and the
/// `updated_at` default are those of the blocking client's `token::Token`: a stored session
/// reads into either type and both write it back byte for byte the same.
#[derive(Clone, Serialize, Deserialize)]
pub struct Token {
    /// Seconds from `updated_at` until the access token expires.
    pub expires_in: u64,
    pub scope: String,
    pub token_type: String,
    pub access_token: String,
    /// The GOG account id, decimal digits.
    pub user_id: String,
    pub refresh_token: String,
    pub session_id: String,
    /// Unix seconds when GOG issued the token; GOG's answer has no such field, so it is set on
    /// receipt.
    #[serde(default = "now")]
    pub updated_at: u64,
}

impl Token {
    #[inline]
    #[must_use]
    pub fn is_expired(&self) -> bool {
        self.updated_at.saturating_add(self.expires_in) <= now()
    }
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_secs())
}

/// Trades a login code for a session.
pub(super) async fn exchange_code(http: &reqwest::Client, code: &str) -> Result<Token, Error> {
    let is_code = !code.is_empty()
        && code.len() <= MAX_CODE_LEN
        && code
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"-_.~".contains(&byte));
    if !is_code {
        return Err(Error::InvalidInput { what: "login code" });
    }
    let request = http.get(Host::Auth.url(TOKEN_PATH)).query(&[
        ("client_id", CLIENT_ID),
        ("client_secret", CLIENT_SECRET),
        ("grant_type", "authorization_code"),
        ("redirect_uri", REDIRECT_URI),
        ("layout", "client2"),
        ("code", code),
    ]);
    checked(http::send(request, &EXCHANGE).await?, &EXCHANGE)
}

/// Trades a refresh token for a new session.
pub(super) async fn refresh(http: &reqwest::Client, refresh_token: &str) -> Result<Token, Error> {
    let request = http.get(Host::Auth.url(TOKEN_PATH)).query(&[
        ("client_id", CLIENT_ID),
        ("client_secret", CLIENT_SECRET),
        ("grant_type", "refresh_token"),
        ("refresh_token", refresh_token),
    ]);
    checked(http::send(request, &REFRESH).await?, &REFRESH)
}

/// The account id becomes a keyring entry and a database key, so it is checked here.
fn checked(token: Token, call: &Call) -> Result<Token, Error> {
    if is_account_id(&token.user_id) {
        Ok(token)
    } else {
        Err(Error::Malformed {
            call: call.name,
            reason: "user_id is not a numeric id".to_owned(),
        })
    }
}

pub(super) fn is_account_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 20 && id.bytes().all(|byte| byte.is_ascii_digit())
}
