//! Whether an account's stored GOG session still works.
//!
//! Asked over decanter's own async client rather than the blocking `api` module's,
//! which never looks at HTTP status codes: a 503 page and a revoked token
//! both reach its callers as "expected value at line 1 column 1". Here they
//! read apart — a session GOG rejected is [`SessionState::Closed`], one that
//! couldn't be asked about is [`SessionState::Unreachable`] — so only the
//! first ever asks the user to log in again.

use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Context as _;
use reqwest::StatusCode;
use serde::Deserialize;
use serde::de::DeserializeOwned;

use crate::api::token::Token;
use crate::auth::{Account, GOG_CLIENT_ID, GOG_CLIENT_SECRET};

const TOKEN_URL: &str = "https://auth.gog.com/token";
const USER_DATA_URL: &str = "https://embed.gog.com/userData.json";

pub enum SessionState {
    Valid(Account),
    /// GOG no longer accepts the session, refresh token included: only a
    /// new browser login brings it back.
    Closed,
    /// GOG couldn't be asked (offline, a timeout, a 5xx): nothing is known
    /// about the session, so nothing about it should change on screen.
    Unreachable(String),
}

/// What one round trip to GOG settled.
enum Answer<T> {
    Accepted(T),
    Rejected,
    Unreachable(String),
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct UserData {
    is_logged_in: bool,
    user_id: Option<String>,
    username: Option<String>,
}

/// Asks GOG whether this account's stored session still works, renewing an
/// expired access token on the way.
///
/// # Errors
///
/// Only a local problem: the keyring, or a stored session that no longer
/// parses. Anything GOG says or fails to say is a [`SessionState`].
pub async fn check_session(account_id: &str) -> anyhow::Result<SessionState> {
    checked(account_id, false).await
}

/// [`check_session`], with the access token renewed first whether or not it
/// has expired — for a user asking to renew a session that just failed.
///
/// # Errors
///
/// The same local problems as [`check_session`].
pub async fn renew_session(account_id: &str) -> anyhow::Result<SessionState> {
    checked(account_id, true).await
}

async fn checked(account_id: &str, force_refresh: bool) -> anyhow::Result<SessionState> {
    let lock = crate::session_lock(account_id);
    let _guard = lock.lock().await;

    let Some(stored) = crate::achv_try!(crate::read_session(account_id).await) else {
        tracing::debug!(account_id, "no GOG session stored, session closed");
        return Ok(SessionState::Closed);
    };
    let token: Token = crate::achv_try!(serde_json::from_str(&stored).context("stored GOG session is corrupt"));
    let client = crate::http_client();

    if force_refresh || is_expired(&token) {
        return refreshed_check(&client, account_id, &token).await;
    }
    match user_data(&client, account_id, &token).await {
        Answer::Accepted(account) => Ok(SessionState::Valid(account)),
        // An access token GOG revoked before its expiry still has a refresh
        // token behind it, which is worth one try.
        Answer::Rejected => refreshed_check(&client, account_id, &token).await,
        Answer::Unreachable(reason) => Ok(SessionState::Unreachable(reason)),
    }
}

async fn refreshed_check(client: &reqwest::Client, account_id: &str, token: &Token) -> anyhow::Result<SessionState> {
    Ok(match crate::achv_try!(refresh(client, account_id, token).await) {
        Answer::Accepted(renewed) => match user_data(client, account_id, &renewed).await {
            Answer::Accepted(account) => SessionState::Valid(account),
            Answer::Rejected => {
                tracing::info!(account_id, "GOG rejected a freshly refreshed session, session closed");
                SessionState::Closed
            }
            Answer::Unreachable(reason) => SessionState::Unreachable(reason),
        },
        Answer::Rejected => SessionState::Closed,
        Answer::Unreachable(reason) => SessionState::Unreachable(reason),
    })
}

fn is_expired(token: &Token) -> bool {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_secs());
    token.updated_at.saturating_add(token.expires_in) <= now
}

/// Trades the refresh token for a new session and stores it straight away,
/// since GOG may rotate the refresh token on use: a renewed token not written
/// back could leave only a spent one in the keyring.
async fn refresh(client: &reqwest::Client, account_id: &str, token: &Token) -> anyhow::Result<Answer<Token>> {
    let request = client.get(TOKEN_URL).query(&[
        ("client_id", GOG_CLIENT_ID),
        ("client_secret", GOG_CLIENT_SECRET),
        ("grant_type", "refresh_token"),
        ("refresh_token", token.refresh_token.as_str()),
    ]);
    let refused = [StatusCode::BAD_REQUEST, StatusCode::UNAUTHORIZED, StatusCode::FORBIDDEN];
    let answer = ask(request, account_id, "GOG token refresh", &refused).await;
    let Answer::Accepted(renewed) = answer else {
        return Ok(answer);
    };
    let serialized = crate::achv_try!(serde_json::to_string(&renewed).context("serializing the refreshed GOG session"));
    crate::achv_try!(crate::write_session(account_id, serialized).await);
    tracing::info!(account_id, "refreshed GOG session");
    Ok(Answer::Accepted(renewed))
}

async fn user_data(client: &reqwest::Client, account_id: &str, token: &Token) -> Answer<Account> {
    let request = client
        .get(USER_DATA_URL)
        .bearer_auth(&token.access_token)
        .header("CSRF", "csrf=true");
    let refused = [StatusCode::UNAUTHORIZED, StatusCode::FORBIDDEN];
    let user: UserData = match ask(request, account_id, "GOG user data request", &refused).await {
        Answer::Accepted(user) => user,
        Answer::Rejected => return Answer::Rejected,
        Answer::Unreachable(reason) => return Answer::Unreachable(reason),
    };
    if !user.is_logged_in {
        tracing::info!(account_id, "GOG user data says not logged in");
        return Answer::Rejected;
    }
    let (Some(user_id), Some(username)) = (user.user_id, user.username) else {
        tracing::warn!(account_id, "GOG user data has no user id or name");
        return Answer::Unreachable("user data without user id or name".to_owned());
    };
    Answer::Accepted(Account {
        account_id: user_id,
        display_name: username,
    })
}

/// Sends `request` and reads a 2xx body as `T`. `refused` are the statuses
/// that mean GOG turned the credentials down; any other failure only means
/// GOG couldn't be asked.
async fn ask<T>(request: reqwest::RequestBuilder, account_id: &str, what: &str, refused: &[StatusCode]) -> Answer<T>
where
    T: DeserializeOwned,
{
    let response = match request.send().await {
        Ok(response) => response,
        Err(err) => return unreachable(account_id, what, err),
    };
    let status = response.status();
    if refused.contains(&status) {
        tracing::info!(
            account_id,
            status = status.as_u16(),
            what,
            "GOG refused the credentials"
        );
        return Answer::Rejected;
    }
    if !status.is_success() {
        tracing::warn!(account_id, status = status.as_u16(), what, "GOG request failed");
        return Answer::Unreachable(format!("{what} failed: HTTP {}", status.as_u16()));
    }
    match response.json().await {
        Ok(body) => Answer::Accepted(body),
        Err(err) => unreachable(account_id, what, err),
    }
}

/// The request URL can carry the refresh token and client secret, and
/// reqwest puts it in its errors.
fn unreachable<T>(account_id: &str, what: &str, err: reqwest::Error) -> Answer<T> {
    let reason = format!("{what} failed: {}", err.without_url());
    tracing::warn!(account_id, reason, "GOG did not answer");
    Answer::Unreachable(reason)
}
