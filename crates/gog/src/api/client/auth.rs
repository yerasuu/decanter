// SPDX-License-Identifier: GPL-3.0-or-later
//! GOG's token endpoint on `auth.gog.com`, spoken as GOG Galaxy speaks it
//! ([gogapidocs: Authentication](https://gogapidocs.readthedocs.io/en/latest/auth.html)).

use std::time::{SystemTime, UNIX_EPOCH};

use decanter_core::secrets::Secret;
use reqwest::StatusCode;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use super::Error;
use super::http::{self, Call, Host};

/// GOG Galaxy's own client id: GOG issues no credentials to third parties, so every open client
/// logs in as Galaxy.
pub const CLIENT_ID: &str = "46899977096215655";
/// Galaxy's client secret as the community documentation publishes it. Public, yet it rides in
/// the token URLs, which is why those requests never reach a log or an error with their URL.
pub const CLIENT_SECRET: &str = "9d85c43b1482497dbbce61f6e4aa173a433796eeae2ca8c5f6129f2dc4de46d9";
/// The only redirect GOG accepts for [`CLIENT_ID`]; the login page lands there with the code.
pub const REDIRECT_URI: &str = "https://embed.gog.com/on_login_success?origin=client";

/// GOG answers a spent or expired grant with 400, so on this endpoint 400 is a refusal of the
/// credentials and not a malformed request.
const GRANT_REFUSED: &[StatusCode] = &[StatusCode::BAD_REQUEST, StatusCode::UNAUTHORIZED, StatusCode::FORBIDDEN];

/// Both grants answer with a fresh token pair, so neither body may be quoted in an error.
const CODE_EXCHANGE: Call = Call {
    name: "login code exchange",
    refused: GRANT_REFUSED,
    max_body: 64 << 10,
    has_secrets: true,
};

const TOKEN_REFRESH: Call = Call {
    name: "token refresh",
    refused: GRANT_REFUSED,
    max_body: 64 << 10,
    has_secrets: true,
};

const CODE_MAX_LEN: usize = 2048;
/// A `u64` written in decimal never needs more digits than this.
const ACCOUNT_ID_MAX_LEN: usize = 20;

/// One account's session, both as GOG's token endpoint answers it and as the keyring stores it.
///
/// The keyring holds this struct serialized with `serde_json`, and the session store compares
/// that text to decide whether to write again: the field order, the names and the absence of any
/// skipping are therefore a storage format, and changing them would rewrite or drop every stored
/// session. The two tokens are [`Secret`]s, written as the plain strings they always were; no
/// `Debug` either, since `session_id` is not one.
#[derive(Serialize, Deserialize)]
pub struct Token {
    /// Seconds from `updated_at` until the access token expires.
    pub expires_in: u64,
    pub scope: String,
    pub token_type: String,
    #[serde(serialize_with = "expose", deserialize_with = "secret")]
    pub access_token: Secret,
    /// The account id; checked to be decimal digits before a token is handed out, since it names
    /// keyring entries, database rows and URL path segments.
    pub user_id: String,
    /// Single use: GOG rotates it on every refresh.
    #[serde(serialize_with = "expose", deserialize_with = "secret")]
    pub refresh_token: Secret,
    pub session_id: String,
    /// Unix seconds of receipt. GOG's answer has no such field, so the default stamps a token as
    /// it is parsed.
    #[serde(default = "unix_now")]
    pub updated_at: u64,
}

impl Token {
    /// No margin is taken off: a token refused just before its expiry is renewed by the one retry
    /// every authorized call gets.
    #[inline]
    #[must_use]
    pub fn is_expired(&self) -> bool {
        self.updated_at.saturating_add(self.expires_in) <= unix_now()
    }
}

impl Clone for Token {
    #[inline]
    fn clone(&self) -> Self {
        Self {
            expires_in: self.expires_in,
            scope: self.scope.clone(),
            token_type: self.token_type.clone(),
            access_token: duplicate(&self.access_token),
            user_id: self.user_id.clone(),
            refresh_token: duplicate(&self.refresh_token),
            session_id: self.session_id.clone(),
            updated_at: self.updated_at,
        }
    }
}

/// A second copy, zeroed on its own drop. `Secret` has no `Clone` so that every copy is
/// spelled out.
pub(super) fn duplicate(secret: &Secret) -> Secret {
    Secret::from(secret.expose().to_owned())
}

fn expose<S>(secret: &Secret, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    serializer.serialize_str(secret.expose())
}

fn secret<'de, D>(deserializer: D) -> Result<Secret, D::Error>
where
    D: Deserializer<'de>,
{
    String::deserialize(deserializer).map(Secret::from)
}

/// A clock set before 1970 reads as 1970, which only makes a token look expired and costs one
/// refresh.
fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_secs())
}

/// Account ids become keyring entry names, database keys and path segments, so only plain
/// decimal digits pass.
pub(super) fn is_account_id(id: &str) -> bool {
    (1..=ACCOUNT_ID_MAX_LEN).contains(&id.len()) && id.bytes().all(|byte| byte.is_ascii_digit())
}

/// Trades the one-shot `code` of GOG's login redirect for a session.
pub(super) async fn exchange_code(http: &reqwest::Client, code: &str) -> Result<Token, Error> {
    if !is_login_code(code) {
        return Err(Error::InvalidInput { what: "login code" });
    }
    let query = [
        ("client_id", CLIENT_ID),
        ("client_secret", CLIENT_SECRET),
        ("grant_type", "authorization_code"),
        ("redirect_uri", REDIRECT_URI),
        ("layout", "client2"),
        ("code", code),
    ];
    request_token(http, &CODE_EXCHANGE, &query).await
}

/// Spends `refresh_token` on a new session, which replaces the old one whole: the refresh token
/// it carries is the only one GOG will accept next time.
pub(super) async fn refresh(http: &reqwest::Client, refresh_token: &Secret) -> Result<Token, Error> {
    let query = [
        ("client_id", CLIENT_ID),
        ("client_secret", CLIENT_SECRET),
        ("grant_type", "refresh_token"),
        ("refresh_token", refresh_token.expose()),
    ];
    request_token(http, &TOKEN_REFRESH, &query).await
}

async fn request_token(http: &reqwest::Client, call: &Call, query: &[(&str, &str)]) -> Result<Token, Error> {
    let token: Token = http::send(http.get(Host::Auth.url("/token")).query(query), call).await?;
    if !is_account_id(&token.user_id) {
        return Err(Error::Malformed {
            call: call.name,
            reason: "user_id is not a numeric id".to_owned(),
        });
    }
    Ok(token)
}

/// The code goes into a query string, so anything beyond the URL-unreserved characters means the
/// caller extracted the wrong part of the pasted text.
fn is_login_code(code: &str) -> bool {
    (1..=CODE_MAX_LEN).contains(&code.len())
        && code
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~'))
}
