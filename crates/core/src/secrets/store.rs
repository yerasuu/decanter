use std::sync::{Arc, PoisonError, RwLock};
use std::time::{Duration, Instant};

use super::{Secret, SecretKey, SecretKind, native};

/// How long a store that could not be reached is left alone before the next call asks again, so
/// that a library listing several accounts does not wait on the same D-Bus timeout once per
/// account.
const RETRY_AFTER: Duration = Duration::from_secs(5);

/// Where secrets are kept.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum BackendKind {
    /// The OS keyring: Secret Service on Linux, Credential Manager on Windows, the Keychain on
    /// macOS.
    Native,
}

/// The store as the last call found it.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Status {
    /// Nothing has needed a secret yet in this run.
    Unknown,
    Ready(BackendKind),
    /// The keyring answered but did not open its collection: locked, or the unlock prompt was
    /// dismissed. Asked again on the next call.
    Locked {
        reason: String,
    },
    /// No keyring to ask: no session bus, no Secret Service provider, no backend for this
    /// platform. Asked again on a call after [`RETRY_AFTER`]. This is where a fallback backend
    /// (decision #1307: a passphrase-encrypted file outside the library) takes over once it
    /// exists, reported as [`Status::Ready`] with its own [`BackendKind`].
    Missing {
        reason: String,
    },
}

/// Why a store call failed. Built from the backend's error kind and its message only, never from
/// what an entry holds, so neither `Debug` nor `Display` can carry a stored value.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum StoreError {
    #[error("secret store unavailable ({kind}): {reason}")]
    Unavailable { kind: SecretKind, reason: String },
    #[error("secret store locked ({kind}): {reason}")]
    Locked { kind: SecretKind, reason: String },
    #[error("secret too large for the store ({kind}): {reason}")]
    TooLarge { kind: SecretKind, reason: String },
    /// The entry exists but does not read back as text; it is left in place.
    #[error("stored secret unreadable ({kind}): {reason}")]
    Corrupt { kind: SecretKind, reason: &'static str },
    #[error("secret store refused the request ({kind}): {reason}")]
    Refused { kind: SecretKind, reason: String },
    /// The account id or NPID cannot name an entry; it is not repeated here, since it may be
    /// whatever was typed.
    #[error("unusable name for a secret ({kind})")]
    InvalidKey { kind: SecretKind },
}

/// A backend's failure, before it is tied to the key it was about.
pub(super) enum Failure {
    /// The connection is gone or never was: the backend is dropped and built again later.
    Unavailable(String),
    Locked(String),
    TooLarge(String),
    Corrupt(&'static str),
    Refused(String),
}

impl Failure {
    pub(super) fn reason(&self) -> &str {
        match self {
            Self::Unavailable(reason) | Self::Locked(reason) | Self::TooLarge(reason) | Self::Refused(reason) => reason,
            Self::Corrupt(reason) => reason,
        }
    }

    fn for_key(self, kind: SecretKind) -> StoreError {
        match self {
            Self::Unavailable(reason) => StoreError::Unavailable { kind, reason },
            Self::Locked(reason) => StoreError::Locked { kind, reason },
            Self::TooLarge(reason) => StoreError::TooLarge { kind, reason },
            Self::Corrupt(reason) => StoreError::Corrupt { kind, reason },
            Self::Refused(reason) => StoreError::Refused { kind, reason },
        }
    }
}

/// One place secrets are kept. Entry names are [`SecretKey::entry`]'s, whatever the backend.
pub(super) trait Backend: Send + Sync {
    fn kind(&self) -> BackendKind;
    fn get(&self, entry: &str) -> Result<Option<Secret>, Failure>;
    fn put(&self, entry: &str, secret: &Secret) -> Result<(), Failure>;
    /// An entry that is not there is already deleted.
    fn delete(&self, entry: &str) -> Result<(), Failure>;
}

struct State {
    backend: Option<Arc<dyn Backend>>,
    status: Status,
    failed_at: Option<Instant>,
}

/// decanter's secrets, behind whichever backend this run could reach.
///
/// The backend is built on first use and dropped when it reports itself unreachable, so a keyring
/// that was locked, missing or not yet started when decanter opened is found by a later call
/// instead of staying off for the whole run. Blocking: async callers use `spawn_blocking`.
pub struct Store {
    state: RwLock<State>,
}

impl Store {
    pub(super) const fn new() -> Self {
        Self {
            state: RwLock::new(State {
                backend: None,
                status: Status::Unknown,
                failed_at: None,
            }),
        }
    }

    #[inline]
    #[must_use]
    pub fn status(&self) -> Status {
        self.state.read().unwrap_or_else(PoisonError::into_inner).status.clone()
    }

    /// `None` when nothing is stored under `key`, the ordinary case of an account nobody logged
    /// in to.
    ///
    /// # Errors
    ///
    /// The store could not be reached or read; nothing is inferred from that about the entry.
    #[inline]
    pub fn get(&self, key: &SecretKey) -> Result<Option<Secret>, StoreError> {
        let result = self.call(key, |backend| {
            let found = backend.get(&key.entry())?;
            if found.is_some() {
                return Ok(found);
            }
            key.legacy_entry()
                .map_or(Ok(None), |legacy| move_legacy(backend, key, &legacy))
        });
        match &result {
            Ok(found) => tracing::debug!(
                kind = ?key.kind(),
                subject = key.subject(),
                found = found.is_some(),
                "read secret from keyring"
            ),
            Err(err) => tracing::error!(
                kind = ?key.kind(),
                subject = key.subject(),
                error = %err,
                "failed to read secret from keyring"
            ),
        }
        result
    }

    /// # Errors
    ///
    /// The value is unfit for `key`, or the store could not be reached or written; the value
    /// was not saved and stays the caller's.
    #[inline]
    pub fn put(&self, key: &SecretKey, secret: &Secret) -> Result<(), StoreError> {
        let result = key.check(secret).and_then(|()| {
            self.call(key, |backend| {
                backend.put(&key.entry(), secret)?;
                // The new value is in; an old-named copy would only linger in the keyring.
                if let Some(legacy) = key.legacy_entry()
                    && let Err(failure) = backend.delete(&legacy)
                {
                    tracing::warn!(kind = ?key.kind(), reason = failure.reason(), "old keyring entry not removed");
                }
                Ok(())
            })
        });
        match &result {
            Ok(()) => tracing::info!(kind = ?key.kind(), subject = key.subject(), "saved secret to keyring"),
            Err(err) => tracing::error!(
                kind = ?key.kind(),
                subject = key.subject(),
                error = %err,
                "failed to save secret to keyring"
            ),
        }
        result
    }

    /// Deleting what was never saved succeeds: the caller wanted it gone, and it is.
    ///
    /// # Errors
    ///
    /// The store could not be reached or the entry removed.
    #[inline]
    pub fn delete(&self, key: &SecretKey) -> Result<(), StoreError> {
        // The old name too, or the next read would bring a forgotten value back from it.
        let result = self.call(key, |backend| {
            backend.delete(&key.entry())?;
            key.legacy_entry().map_or(Ok(()), |legacy| backend.delete(&legacy))
        });
        match &result {
            Ok(()) => tracing::info!(kind = ?key.kind(), subject = key.subject(), "deleted secret from keyring"),
            Err(err) => tracing::error!(
                kind = ?key.kind(),
                subject = key.subject(),
                error = %err,
                "failed to delete secret from keyring"
            ),
        }
        result
    }

    fn call<T>(&self, key: &SecretKey, op: impl FnOnce(&dyn Backend) -> Result<T, Failure>) -> Result<T, StoreError> {
        let backend = self.backend().map_err(|failure| failure.for_key(key.kind()))?;
        let result = op(backend.as_ref());
        let mut state = self.state.write().unwrap_or_else(PoisonError::into_inner);
        match &result {
            Ok(_) => state.status = Status::Ready(backend.kind()),
            Err(Failure::Unavailable(reason)) => {
                tracing::warn!(%reason, "keyring connection lost, reconnecting on a later call");
                state.status = Status::Missing { reason: reason.clone() };
                state.failed_at = Some(Instant::now());
                if state
                    .backend
                    .as_ref()
                    .is_some_and(|current| Arc::ptr_eq(current, &backend))
                {
                    state.backend = None;
                }
            }
            Err(Failure::Locked(reason)) => state.status = Status::Locked { reason: reason.clone() },
            Err(Failure::TooLarge(_) | Failure::Corrupt(_) | Failure::Refused(_)) => {}
        }
        drop(state);
        result.map_err(|failure| failure.for_key(key.kind()))
    }

    fn backend(&self) -> Result<Arc<dyn Backend>, Failure> {
        if let Some(backend) = &self.state.read().unwrap_or_else(PoisonError::into_inner).backend {
            return Ok(Arc::clone(backend));
        }
        // Held across the connection so that concurrent first calls connect once.
        let mut state = self.state.write().unwrap_or_else(PoisonError::into_inner);
        if let Some(backend) = &state.backend {
            return Ok(Arc::clone(backend));
        }
        if let (Some(failed_at), Status::Missing { reason }) = (state.failed_at, &state.status)
            && failed_at.elapsed() < RETRY_AFTER
        {
            return Err(Failure::Unavailable(reason.clone()));
        }
        let connected = native::connect();
        match connected {
            Ok(native) => {
                let backend: Arc<dyn Backend> = Arc::new(native);
                state.status = Status::Ready(backend.kind());
                state.failed_at = None;
                state.backend = Some(Arc::clone(&backend));
                drop(state);
                tracing::info!(backend = ?backend.kind(), "keyring ready");
                Ok(backend)
            }
            Err(reason) => {
                state.status = Status::Missing { reason: reason.clone() };
                state.failed_at = Some(Instant::now());
                drop(state);
                tracing::warn!(%reason, "no keyring reachable, secrets cannot be read or saved until it is");
                Err(Failure::Unavailable(reason))
            }
        }
    }
}

/// Copies the secret filed under its old name to its own, and removes the old entry only once the
/// new one reads back the same. Whatever fails leaves the old entry in place for the next read to
/// try again, and the value read is the caller's either way.
fn move_legacy(backend: &dyn Backend, key: &SecretKey, legacy: &str) -> Result<Option<Secret>, Failure> {
    let Some(old) = backend.get(legacy)? else {
        return Ok(None);
    };
    let entry = key.entry();
    let moved = backend
        .put(&entry, &old)
        .and_then(|()| backend.get(&entry))
        .map(|back| back.is_some_and(|back| back.expose() == old.expose()));
    match moved {
        Ok(true) => match backend.delete(legacy) {
            Ok(()) => tracing::info!(kind = ?key.kind(), "moved secret to its own keyring entry"),
            Err(failure) => tracing::warn!(
                kind = ?key.kind(),
                reason = failure.reason(),
                "moved secret to its own keyring entry, the old entry is removed on a later read"
            ),
        },
        Ok(false) => {
            // A copy that reads back otherwise would shadow the good old one from now on.
            let removed = backend.delete(&entry).is_ok();
            tracing::warn!(
                kind = ?key.kind(),
                removed,
                "secret read back otherwise under its own keyring entry, kept the old entry"
            );
        }
        Err(failure) => tracing::warn!(
            kind = ?key.kind(),
            reason = failure.reason(),
            "could not move secret to its own keyring entry, kept the old entry"
        ),
    }
    Ok(Some(old))
}
