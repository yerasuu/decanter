//! Whether an account's stored GOG session still works.
//!
//! Asked over the async [`Client`], which reads HTTP status codes apart from
//! the body: a 503 page and a revoked token no longer both reach the caller
//! as "expected value at line 1 column 1". Here they read apart — a session GOG rejected is [`SessionState::Closed`], one that
//! couldn't be asked about is [`SessionState::Unreachable`] — so only the
//! first ever asks the user to log in again.

use anyhow::Context as _;

use crate::api::client::{Client, Error, Token};
use crate::auth::Account;

pub enum SessionState {
    Valid(Account),
    /// GOG no longer accepts the session, refresh token included: only a
    /// new browser login brings it back.
    Closed,
    /// GOG couldn't be asked (offline, a timeout, a 5xx): nothing is known
    /// about the session, so nothing about it should change on screen.
    Unreachable(String),
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
    let answer = crate::achv_try!(
        locked(account_id, async |client| {
            if force_refresh {
                crate::achv_try!(client.refresh().await);
            }
            client.user().await
        })
        .await
    );
    let Some(answer) = answer else {
        tracing::debug!(account_id, "no GOG session stored, session closed");
        return Ok(SessionState::Closed);
    };
    match answer {
        Ok(user) => Ok(SessionState::Valid(Account {
            account_id: user.id,
            display_name: user.username,
        })),
        Err(err) => state_after(account_id, err),
    }
}

/// Only a refusal closes the session: a non-parseable answer says nothing
/// about the credentials, so it reads as GOG not being reachable.
fn state_after(account_id: &str, err: Error) -> anyhow::Result<SessionState> {
    match err {
        Error::Rejected { call, reason } => {
            tracing::info!(account_id, call, reason, "GOG refused the session, session closed");
            Ok(SessionState::Closed)
        }
        Error::Network { .. } | Error::Unavailable { .. } | Error::Malformed { .. } => {
            let reason = format!("{:#}", anyhow::Error::new(err));
            tracing::warn!(account_id, reason, "GOG did not answer the session check");
            Ok(SessionState::Unreachable(reason))
        }
        Error::InvalidInput { .. } | Error::Setup(_) => Err(anyhow::Error::new(err)),
    }
}

/// Runs `call` against a [`Client`] built from this account's stored session
/// and stores the session it ends with, then hands back what `call` returned.
///
/// # Errors
///
/// No session stored, or a local problem: the keyring, a stored session that
/// no longer parses, the HTTP client. What GOG answered is `call`'s to report.
pub(crate) async fn with_client<T>(
    account_id: &str,
    call: impl AsyncFnOnce(&Client) -> anyhow::Result<T>,
) -> anyhow::Result<T> {
    let answer = crate::achv_try!(locked(account_id, call).await);
    crate::achv_try!(answer.context("no GOG session for this account — log in first"))
}

/// The session as GOG still accepts it, for the requests decanter sends
/// itself with its access token (content-system, CDN, cloud storage, remote
/// config): asking for the user renews an expired access token first and
/// turns a refused session into an error here rather than at those hosts.
///
/// # Errors
///
/// As [`with_client`], plus GOG refusing the session or not answering.
pub(crate) async fn live_token(account_id: &str) -> anyhow::Result<Token> {
    with_client(account_id, async |client| {
        crate::achv_try!(
            client
                .user()
                .await
                .map_err(|err| failed("GOG session is not valid", err))
        );
        Ok(client.token().await)
    })
    .await
}

/// [`with_client`] for callers that read GOG's answer themselves; `None` when
/// no session is stored.
///
/// The session is stored again after every call, failed ones included: GOG
/// rotates the refresh token on use, and a renewal can succeed before the
/// request that then fails, so skipping the store would leave only a spent
/// refresh token in the keyring. Load, call and store all run under the
/// account's [`crate::session_lock`], so that no two callers start from the
/// same stored refresh token; the [`Client`]'s own renewal lock
/// then covers whatever `call` runs concurrently on it.
async fn locked<T>(account_id: &str, call: impl AsyncFnOnce(&Client) -> T) -> anyhow::Result<Option<T>> {
    let lock = crate::session_lock(account_id);
    let _guard = lock.lock().await;

    let Some(stored) = crate::achv_try!(crate::read_session(account_id).await) else {
        tracing::debug!(account_id, "no GOG session stored for this account");
        return Ok(None);
    };
    let token: Token = crate::achv_try!(serde_json::from_str(&stored).context("stored GOG session is corrupt"));
    let client = crate::achv_try!(Client::new(token).context("building the GOG HTTP client"));

    let answer = call(&client).await;

    let ended = crate::achv_try!(serde_json::to_string(&client.token().await).context("serializing the GOG session"));
    if ended != stored {
        crate::achv_try!(crate::write_session(account_id, ended).await);
        tracing::info!(account_id, "stored the renewed GOG session");
    }
    Ok(Some(answer))
}

/// One message with the whole cause chain, since callers show only the
/// outermost one.
pub(crate) fn failed(what: &str, err: Error) -> anyhow::Error {
    anyhow::anyhow!("{what}: {:#}", anyhow::Error::new(err))
}
