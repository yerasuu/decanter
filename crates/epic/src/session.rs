// SPDX-License-Identifier: GPL-3.0-or-later
//! An Epic account's session as the keyring stores it (`SecretKind::EpicSession`), and the calls
//! that check, renew and drop it, by the lifecycle rules of spec section 3.5.

use std::collections::HashMap;
use std::sync::{Arc, Mutex as StdMutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Context as _;
use decanter_core::secrets::{self, Secret, SecretKey};
use serde::{Deserialize, Serialize, Serializer};
use tokio::sync::Mutex as AsyncMutex;

use crate::error::CallError;
use crate::http;
use crate::token::{self, Grant};

/// The launcher reuses an access token only while more than ten minutes of it remain (spec, 3.5
/// item 2); past that it renews, so a token does not run out in the middle of a long call.
const REUSE_MARGIN_SECS: u64 = 600;

/// Who a stored session belongs to.
#[derive(Debug)]
#[non_exhaustive]
pub struct Account {
    /// Epic's `account_id`, checked by `SecretKey::epic_session`.
    pub account_id: String,
    /// Epic's `displayName`, or the account id when Epic gives none.
    pub display_name: String,
}

/// What Epic says about an account's session.
#[derive(Debug)]
#[non_exhaustive]
pub enum SessionState {
    Valid(Account),
    /// Epic refused the session, refresh token included, or none is stored: only a new browser
    /// login brings it back.
    Closed,
    /// Epic could not be asked (offline, a timeout, a moved endpoint, a 5xx, an unreadable
    /// answer): nothing is known about the session, so nothing about it should change on screen.
    Unreachable(String),
}

/// One account's session, as the keyring holds it serialized with `serde_json`.
///
/// The field names are a storage format: renaming one drops every stored session. The tokens are
/// written as the plain strings Epic issued.
#[derive(Serialize, Deserialize)]
pub(crate) struct Session {
    account_id: String,
    display_name: String,
    #[serde(serialize_with = "expose", deserialize_with = "token::secret")]
    access_token: Secret,
    #[serde(serialize_with = "expose", deserialize_with = "token::secret")]
    refresh_token: Secret,
    /// Unix seconds.
    expires_at: u64,
}

impl Session {
    pub(crate) fn granted(grant: Grant) -> Self {
        Self {
            account_id: grant.account_id,
            display_name: grant.display_name,
            access_token: grant.access_token,
            refresh_token: grant.refresh_token,
            expires_at: unix_now().saturating_add(grant.expires_in),
        }
    }

    pub(crate) fn account(&self) -> Account {
        Account {
            account_id: self.account_id.clone(),
            display_name: self.display_name.clone(),
        }
    }

    fn is_reusable(&self) -> bool {
        self.expires_at > unix_now().saturating_add(REUSE_MARGIN_SECS)
    }

    fn is_same_grant(&self, other: &Self) -> bool {
        self.refresh_token.expose() == other.refresh_token.expose()
    }

    /// The keyring is outside this process: what comes back is checked as what Epic sent was.
    fn is_sound(&self, account_id: &str) -> bool {
        self.account_id == account_id && token::is_token(&self.access_token) && token::is_token(&self.refresh_token)
    }
}

/// Asks Epic whether this account's stored session still works: verify while the access token
/// has time left, a renewal otherwise or when verify fails (spec, 3.5 item 3).
///
/// # Errors
///
/// Only a local problem: the keyring, a stored session that does not parse, the HTTP client.
/// Anything Epic says or fails to say is a [`SessionState`].
#[inline]
pub async fn check_session(account_id: &str) -> anyhow::Result<SessionState> {
    let Some(stored) = load(account_id).await? else {
        tracing::debug!(account_id, "no epic session stored, session closed");
        return Ok(SessionState::Closed);
    };
    let http = http::client()?;
    if stored.is_reusable() {
        match token::verify(&http, &stored.access_token, account_id).await {
            Ok(()) => return Ok(SessionState::Valid(stored.account())),
            Err(err) => tracing::debug!(account_id, error = %err, "epic token verify failed, renewing the session"),
        }
    }
    renewed(&http, &stored).await
}

/// Drops the stored session. Epic is not asked to end it: the launcher never does, and decanter
/// calls no endpoint the launcher does not (analysis, D1).
///
/// # Errors
///
/// The keyring failing to drop the session.
#[inline]
pub async fn log_out(account_id: &str) -> anyhow::Result<()> {
    let lock = session_lock(account_id);
    let _guard = lock.lock().await;
    clear(account_id).await?;
    tracing::info!(account_id, "logged out of epic account");
    Ok(())
}

/// Stores `fresh` as its account's session, whatever was stored before: a new login is what the
/// user asked for.
pub(crate) async fn replace(fresh: &Session) -> anyhow::Result<()> {
    let lock = session_lock(&fresh.account_id);
    let _guard = lock.lock().await;
    store(fresh).await
}

/// Renews the session `started` was read as, under the account's lock so that only one renewal
/// spends a refresh token at a time (spec, 3.6). The lock is held across the token call alone,
/// which the client's time limit bounds.
async fn renewed(http: &reqwest::Client, started: &Session) -> anyhow::Result<SessionState> {
    let account_id = started.account_id.as_str();
    let lock = session_lock(account_id);
    let _guard = lock.lock().await;
    let Some(current) = load(account_id).await? else {
        return Ok(SessionState::Closed);
    };
    if !current.is_same_grant(started) && current.is_reusable() {
        tracing::debug!(account_id, "another caller renewed the epic session meanwhile");
        return Ok(SessionState::Valid(current.account()));
    }
    let failure = match token::refresh(http, &current.refresh_token).await {
        Ok(grant) if grant.account_id == account_id => {
            let renewed = Session::granted(grant);
            swap(&current, &renewed).await?;
            return Ok(SessionState::Valid(renewed.account()));
        }
        Ok(_) => CallError::Malformed {
            call: "token refresh",
            reason: "the renewed session belongs to another account".to_owned(),
        },
        Err(err) => err,
    };
    match failure {
        CallError::Revoked { .. } | CallError::ActionRequired { .. } => {
            tracing::info!(account_id, error = %failure, "epic refused the session, session closed");
            // The spent session is of no use any more, and a login from the browser replaces it
            // (spec, 3.5 item 4).
            if load(account_id)
                .await?
                .is_some_and(|stored| stored.is_same_grant(&current))
            {
                clear(account_id).await?;
            }
            Ok(SessionState::Closed)
        }
        CallError::TimedOut { .. }
        | CallError::Unreachable { .. }
        | CallError::Moved { .. }
        | CallError::Unavailable { .. }
        | CallError::Malformed { .. } => {
            let reason = reason(failure);
            tracing::warn!(account_id, reason, "epic did not answer the token refresh");
            Ok(SessionState::Unreachable(reason))
        }
        CallError::Setup(_) => Err(anyhow::Error::new(failure)),
    }
}

/// Stores `renewed` only if the keyring still holds the session it was renewed from. The lock
/// covers this process only; another decanter (the CLI) that stored a newer session meanwhile
/// keeps it, as GOG's renewal does.
async fn swap(current: &Session, renewed: &Session) -> anyhow::Result<()> {
    let account_id = current.account_id.as_str();
    if load(account_id)
        .await?
        .is_some_and(|stored| stored.is_same_grant(current))
    {
        store(renewed).await?;
        tracing::info!(account_id, "stored the renewed epic session");
    } else {
        tracing::debug!(account_id, "another caller stored a newer epic session, keeping it");
    }
    Ok(())
}

/// One message with the whole cause chain, since callers show only the outermost one.
pub(crate) fn failed(what: &str, err: CallError) -> anyhow::Error {
    anyhow::anyhow!("{what}: {}", reason(err))
}

fn reason(err: CallError) -> String {
    format!("{:#}", anyhow::Error::new(err))
}

/// The account's stored session, read in `spawn_blocking` since the keyring is a blocking D-Bus
/// round trip.
async fn load(account_id: &str) -> anyhow::Result<Option<Session>> {
    let key = SecretKey::epic_session(account_id)?;
    let found = tokio::task::spawn_blocking(move || secrets::store().get(&key))
        .await
        .context("session-read worker task panicked")?
        .context("reading the Epic session from the keyring")?;
    let Some(stored) = found else {
        tracing::debug!(account_id, "no epic session stored for this account");
        return Ok(None);
    };
    // serde_json quotes the offending value in its message, and here that value is part of a
    // stored session: only where it failed is kept.
    let session: Session = serde_json::from_str(stored.expose()).map_err(|err| {
        anyhow::anyhow!(
            "stored Epic session is corrupt: {:?} error at line {} column {}",
            err.classify(),
            err.line(),
            err.column()
        )
    })?;
    anyhow::ensure!(
        session.is_sound(account_id),
        "stored Epic session is corrupt: a field fails its check"
    );
    Ok(Some(session))
}

async fn store(session: &Session) -> anyhow::Result<()> {
    let key = SecretKey::epic_session(&session.account_id)?;
    let serialized = Secret::from(serde_json::to_string(session).context("serializing the Epic session")?);
    tokio::task::spawn_blocking(move || secrets::store().put(&key, &serialized))
        .await
        .context("session-write worker task panicked")?
        .context("writing the Epic session to the keyring")?;
    tracing::debug!(account_id = %session.account_id, "wrote epic session to keyring");
    Ok(())
}

async fn clear(account_id: &str) -> anyhow::Result<()> {
    let key = SecretKey::epic_session(account_id)?;
    tokio::task::spawn_blocking(move || secrets::store().delete(&key))
        .await
        .context("session-clear worker task panicked")?
        .context("clearing the Epic session from the keyring")?;
    tracing::debug!(account_id, "cleared epic session from keyring");
    Ok(())
}

/// One lock per account, from a registry whose own lock is held only to look an entry up.
///
/// Epic may rotate the refresh token on every renewal (spec, 3.5 item 5), so two renewals racing
/// from the same stored token can leave the keyring holding the one Epic just invalidated. Like
/// GOG's, the lock covers this process only; [`swap`] covers the rest.
fn session_lock(account_id: &str) -> Arc<AsyncMutex<()>> {
    static LOCKS: OnceLock<StdMutex<Locks>> = OnceLock::new();
    let registry = LOCKS.get_or_init(|| StdMutex::new(HashMap::new()));
    let mut locks = registry.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    Arc::clone(
        locks
            .entry(account_id.to_owned())
            .or_insert_with(|| Arc::new(AsyncMutex::new(()))),
    )
}

type Locks = HashMap<String, Arc<AsyncMutex<()>>>;

fn expose<S>(secret: &Secret, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    serializer.serialize_str(secret.expose())
}

/// A clock set before 1970 reads as 1970, which only makes a token look expired and costs one
/// renewal.
fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_secs())
}
