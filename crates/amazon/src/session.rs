// SPDX-License-Identifier: GPL-3.0-or-later
//! An Amazon account's session as the keyring stores it (`SecretKind::AmazonSession`, decision
//! #1518, D4), and the calls that read, renew and drop it.

use std::collections::HashMap;
use std::sync::{Arc, Mutex as StdMutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Context as _;
use decanter_core::secrets::{self, Secret, SecretKey};
use serde::{Deserialize, Serialize, Serializer};
use tokio::sync::Mutex as AsyncMutex;

use crate::device::{self, Registration, Renewal};
use crate::error::CallError;
use crate::http;

/// nile renews only once a token has expired (analysis, section 2.2); renewing this early keeps a
/// token from expiring between the check and the request that bears it.
const REFRESH_MARGIN_SECS: u64 = 5 * 60;

/// Who a stored session belongs to.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct Account {
    /// Amazon's `customer_info.user_id`, checked by `SecretKey::amazon_session`.
    pub account_id: String,
    /// `customer_info.given_name`, or the account id when Amazon gives none.
    pub display_name: String,
}

/// What the keyring says about an account's session, without asking Amazon.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum SessionState {
    LoggedOut,
    Valid,
    /// The access token has expired or is about to; [`live_session`] renews it.
    NeedsRefresh,
    /// Amazon refused the refresh token: only a new browser login brings the session back.
    Refused,
}

/// A session ready for a request, for the library and download calls of the next stages.
#[derive(Debug)]
#[non_exhaustive]
pub struct LiveSession {
    pub account_id: String,
    pub access_token: Secret,
    /// Amazon's calls past the auth service name the device by these (analysis, section 2.4).
    pub device_serial: String,
    pub client_id: String,
}

/// What [`live_session`] found.
#[derive(Debug)]
#[non_exhaustive]
pub enum Live {
    Ready(LiveSession),
    LoggedOut,
    Refused,
    /// Amazon could not be asked (offline, a timeout, a 5xx, an unreadable answer): nothing is
    /// known about the session, so nothing about it should change on screen.
    Unreachable(String),
}

/// Whether logging out also removed the device from the Amazon account.
#[derive(Debug)]
#[non_exhaustive]
pub enum Deregistration {
    Done,
    /// No session was stored for the account.
    NotStored,
    /// Amazon no longer accepts the session, so it cannot be asked to remove the device; the user
    /// removes it from their Amazon account's devices page.
    Refused,
    /// Amazon could not be asked; the device stays registered on the account.
    Unreachable(String),
}

/// One account's session, as the keyring holds it serialized with `serde_json`.
///
/// The field names are a storage format: renaming one drops every stored session. The tokens are
/// written as the plain strings Amazon issued.
#[derive(Serialize, Deserialize)]
pub(crate) struct Session {
    account_id: String,
    display_name: String,
    #[serde(serialize_with = "expose", deserialize_with = "device::secret")]
    access_token: Secret,
    #[serde(serialize_with = "expose", deserialize_with = "device::secret")]
    refresh_token: Secret,
    /// Unix seconds.
    expires_at: u64,
    device_serial: String,
    client_id: String,
    /// Set once Amazon refuses the refresh token, so that a dead session is not sent to Amazon
    /// again on every check until the user logs in anew.
    refused: bool,
}

impl Session {
    pub(crate) fn registered(registration: Registration, device_serial: String, client_id: String) -> Self {
        Self {
            account_id: registration.account_id,
            display_name: registration.display_name,
            access_token: registration.access_token,
            refresh_token: registration.refresh_token,
            expires_at: unix_now().saturating_add(registration.expires_in),
            device_serial,
            client_id,
            refused: false,
        }
    }

    fn needs_refresh(&self) -> bool {
        self.expires_at <= unix_now().saturating_add(REFRESH_MARGIN_SECS)
    }

    fn state(&self) -> SessionState {
        if self.refused {
            SessionState::Refused
        } else if self.needs_refresh() {
            SessionState::NeedsRefresh
        } else {
            SessionState::Valid
        }
    }

    fn renew(&mut self, renewal: Renewal) {
        self.access_token = renewal.access_token;
        self.expires_at = unix_now().saturating_add(renewal.expires_in);
    }

    fn live(&self) -> LiveSession {
        LiveSession {
            account_id: self.account_id.clone(),
            access_token: Secret::from(self.access_token.expose().to_owned()),
            device_serial: self.device_serial.clone(),
            client_id: self.client_id.clone(),
        }
    }

    /// The keyring is outside this process: what comes back is checked as what Amazon sent was.
    fn is_sound(&self, account_id: &str) -> bool {
        self.account_id == account_id
            && device::is_token(&self.access_token)
            && device::is_token(&self.refresh_token)
            && self.device_serial.len() == 32
            && self.device_serial.bytes().all(|byte| byte.is_ascii_hexdigit())
            && (1..=256).contains(&self.client_id.len())
            && self.client_id.bytes().all(|byte| byte.is_ascii_hexdigit())
    }
}

/// The session's state as stored, without asking Amazon or waiting for a renewal in progress.
///
/// # Errors
///
/// The keyring failing, or a stored session that does not parse.
#[inline]
pub async fn session_state(account_id: &str) -> anyhow::Result<SessionState> {
    Ok(load(account_id)
        .await?
        .map_or(SessionState::LoggedOut, |session| session.state()))
}

/// The account's session with an access token good for a few more minutes, renewed first when it
/// is not.
///
/// # Errors
///
/// Only a local problem: the keyring, a stored session that does not parse, the HTTP client.
/// Anything Amazon says or fails to say is a [`Live`].
#[inline]
pub async fn live_session(account_id: &str) -> anyhow::Result<Live> {
    let lock = session_lock(account_id);
    let _guard = lock.lock().await;
    let Some(mut session) = load(account_id).await? else {
        return Ok(Live::LoggedOut);
    };
    match session.state() {
        SessionState::LoggedOut => return Ok(Live::LoggedOut),
        SessionState::Refused => return Ok(Live::Refused),
        SessionState::Valid => return Ok(Live::Ready(session.live())),
        SessionState::NeedsRefresh => {}
    }
    let http = http::client()?;
    match renewed(&http, &mut session).await {
        Ok(()) => Ok(Live::Ready(session.live())),
        Err(Failure::Refused) => Ok(Live::Refused),
        Err(Failure::Unreachable(reason)) => Ok(Live::Unreachable(reason)),
        Err(Failure::Local(err)) => Err(err),
    }
}

/// Removes the device from the Amazon account when Amazon can still be asked, then drops the
/// stored session whatever Amazon answered: a user logging out is not kept logged in by an
/// offline machine.
///
/// # Errors
///
/// The keyring failing to read or to drop the session.
#[inline]
pub async fn log_out(account_id: &str) -> anyhow::Result<Deregistration> {
    let lock = session_lock(account_id);
    let _guard = lock.lock().await;
    let Some(mut session) = load(account_id).await? else {
        return Ok(Deregistration::NotStored);
    };
    let outcome = match http::client() {
        Ok(http) => deregistered(&http, &mut session).await,
        Err(err) => Deregistration::Unreachable(reason(err)),
    };
    clear(account_id).await?;
    tracing::info!(
        account_id,
        deregistered = matches!(outcome, Deregistration::Done),
        "logged out of amazon account"
    );
    Ok(outcome)
}

/// Stores `fresh` as the account's session, deregistering the device of the one it replaces.
pub(crate) async fn replace(http: &reqwest::Client, account_id: &str, fresh: &Session) -> anyhow::Result<()> {
    let lock = session_lock(account_id);
    let _guard = lock.lock().await;
    // A stored session that no longer parses is replaced all the same: the new login is what the
    // user asked for.
    if let Ok(Some(mut old)) = load(account_id).await {
        let outcome = deregistered(http, &mut old).await;
        if !matches!(outcome, Deregistration::Done) {
            tracing::warn!(account_id, "previous amazon device left registered after a new login");
        }
    }
    store(fresh).await
}

enum Failure {
    Refused,
    Unreachable(String),
    Local(anyhow::Error),
}

/// Renews `session`'s access token and stores the outcome; a refusal is stored too, as
/// [`Session::refused`].
async fn renewed(http: &reqwest::Client, session: &mut Session) -> Result<(), Failure> {
    let failure = match device::refresh(http, &session.refresh_token).await {
        Ok(renewal) => {
            session.renew(renewal);
            store(session).await.map_err(Failure::Local)?;
            tracing::info!(account_id = %session.account_id, "stored the renewed amazon session");
            return Ok(());
        }
        Err(err) => err,
    };
    match failure {
        CallError::Rejected { call, status } => {
            tracing::info!(account_id = %session.account_id, call, status, "amazon refused the session");
            session.refused = true;
            store(session).await.map_err(Failure::Local)?;
            Err(Failure::Refused)
        }
        CallError::Network { .. } | CallError::Unavailable { .. } | CallError::Malformed { .. } => {
            let reason = reason(failure);
            tracing::warn!(account_id = %session.account_id, reason, "amazon did not answer the token refresh");
            Err(Failure::Unreachable(reason))
        }
        CallError::Setup(_) | CallError::Random(_) => Err(Failure::Local(anyhow::Error::new(failure))),
    }
}

/// Deregisters `session`'s device, renewing its access token first when it has expired.
async fn deregistered(http: &reqwest::Client, session: &mut Session) -> Deregistration {
    if session.refused {
        return Deregistration::Refused;
    }
    if session.needs_refresh() {
        match renewed(http, session).await {
            Ok(()) => {}
            Err(Failure::Refused) => return Deregistration::Refused,
            Err(Failure::Unreachable(reason)) => return Deregistration::Unreachable(reason),
            Err(Failure::Local(err)) => return Deregistration::Unreachable(format!("{err:#}")),
        }
    }
    match device::deregister(http, &session.access_token).await {
        Ok(()) => Deregistration::Done,
        Err(CallError::Rejected { .. }) => Deregistration::Refused,
        Err(err) => Deregistration::Unreachable(reason(err)),
    }
}

/// One message with the whole cause chain, since callers show only the outermost one.
pub(crate) fn failed(what: &str, err: CallError) -> anyhow::Error {
    anyhow::anyhow!("{what}: {}", reason(err))
}

pub(crate) fn reason(err: CallError) -> String {
    format!("{:#}", anyhow::Error::new(err))
}

/// The account's stored session, read in `spawn_blocking` since the keyring is a blocking D-Bus
/// round trip.
async fn load(account_id: &str) -> anyhow::Result<Option<Session>> {
    let key = SecretKey::amazon_session(account_id)?;
    let found = tokio::task::spawn_blocking(move || secrets::store().get(&key))
        .await
        .context("session-read worker task panicked")?
        .context("reading the Amazon session from the keyring")?;
    let Some(stored) = found else {
        tracing::debug!(account_id, "no amazon session stored for this account");
        return Ok(None);
    };
    // serde_json quotes the offending value in its message, and here that value is part of a
    // stored session: only where it failed is kept.
    let session: Session = serde_json::from_str(stored.expose()).map_err(|err| {
        anyhow::anyhow!(
            "stored Amazon session is corrupt: {:?} error at line {} column {}",
            err.classify(),
            err.line(),
            err.column()
        )
    })?;
    anyhow::ensure!(
        session.is_sound(account_id),
        "stored Amazon session is corrupt: a field fails its check"
    );
    Ok(Some(session))
}

async fn store(session: &Session) -> anyhow::Result<()> {
    let key = SecretKey::amazon_session(&session.account_id)?;
    let serialized = Secret::from(serde_json::to_string(session).context("serializing the Amazon session")?);
    tokio::task::spawn_blocking(move || secrets::store().put(&key, &serialized))
        .await
        .context("session-write worker task panicked")?
        .context("writing the Amazon session to the keyring")?;
    tracing::debug!(account_id = %session.account_id, "wrote amazon session to keyring");
    Ok(())
}

async fn clear(account_id: &str) -> anyhow::Result<()> {
    let key = SecretKey::amazon_session(account_id)?;
    tokio::task::spawn_blocking(move || secrets::store().delete(&key))
        .await
        .context("session-clear worker task panicked")?
        .context("clearing the Amazon session from the keyring")?;
    tracing::debug!(account_id, "cleared amazon session from keyring");
    Ok(())
}

/// One lock per account, from a registry whose own lock is held only to look an entry up.
///
/// Amazon does not rotate the refresh token (analysis, section 2.2), so two renewals racing cost
/// only a second request, not the session; serializing them still keeps a refusal stored by one
/// caller from being overwritten by another's stale copy, and keeps decanter from sending Amazon
/// more than one renewal at a time. Like GOG's, the lock covers this process only.
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
/// refresh.
fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_secs())
}
