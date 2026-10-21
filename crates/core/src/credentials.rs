//! Storefront sessions and shadNet secrets, in the OS keyring.
//!
//! One entry per `(account, storefront)`, holding whatever that
//! storefront's client needs to resume a session — for GOG, a serialized
//! `gog::token::Token`. Never on disk in the clear: these are live
//! credentials to someone's game library and payment account. psport's
//! online play signs in to a shadNet server, which no storefront owns: its
//! password and token are filed under `shadnet/<NPID>/…` (decision #715).
//!
//! Blocking on purpose. The Secret Service is a D-Bus round trip, called
//! a handful of times per session; async callers wrap these in
//! `spawn_blocking` rather than this crate taking on a runtime.

#![expect(
    clippy::std_instead_of_core,
    reason = "desktop app, no no_std target exists to justify core:: here"
)]
#![expect(
    clippy::implicit_return,
    reason = "directly contradicts needless_return from clippy::all, also enabled"
)]
#![expect(
    clippy::large_stack_frames,
    reason = "a couple of Strings plus a keyring::Error already cost more than the 512-byte threshold"
)]
#![expect(
    clippy::missing_errors_doc,
    reason = "every fallible fn here fails for the same reason (the OS keyring backend failed); \
              a # Errors section repeating that is exactly the restates-the-signature filler \
              this codebase's own humanization scanner rejects"
)]

use std::result::Result as StdResult;

use keyring::Entry;

use crate::catalog::Storefront;

/// Keyring service name for decanter's own entries.
const SERVICE: &str = "dev.decanter.Decanter";

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum KeyringError {
    #[error("keyring: {0}")]
    Keyring(#[from] keyring::Error),
}

pub type Result<T> = StdResult<T, KeyringError>;

/// The account half of a keyring entry.
fn account(account_id: &str, storefront: Storefront) -> String {
    format!("{account_id}/{}", storefront.as_str_name())
}

/// `None` means nobody is logged in for this account and storefront — the
/// ordinary case, not an error.
#[inline]
pub fn load(account_id: &str, storefront: Storefront) -> Result<Option<String>> {
    let result = read(&account(account_id, storefront));
    // Never log the secret itself — only whether one was found.
    match &result {
        Ok(Some(_)) => tracing::debug!(account_id, ?storefront, "loaded session from keyring"),
        Ok(None) => tracing::debug!(account_id, ?storefront, "no session in keyring"),
        Err(err) => tracing::error!(account_id, ?storefront, error = %err, "failed to load session from keyring"),
    }
    result
}

#[inline]
pub fn save(account_id: &str, storefront: Storefront, secret: &str) -> Result<()> {
    let entry = match Entry::new(SERVICE, &account(account_id, storefront)) {
        Ok(entry) => entry,
        Err(err) => {
            tracing::error!(account_id, ?storefront, error = %err, "failed to open keyring entry for save");
            return Err(err.into());
        }
    };
    match entry.set_password(secret) {
        Ok(()) => {
            tracing::debug!(account_id, ?storefront, "saved session to keyring");
            Ok(())
        }
        Err(err) => {
            tracing::error!(account_id, ?storefront, error = %err, "failed to save session to keyring");
            Err(err.into())
        }
    }
}

/// Deleting a session nobody has is a success: the caller wanted it gone,
/// and it is.
#[inline]
pub fn delete(account_id: &str, storefront: Storefront) -> Result<()> {
    let entry = match Entry::new(SERVICE, &account(account_id, storefront)) {
        Ok(entry) => entry,
        Err(err) => {
            tracing::error!(account_id, ?storefront, error = %err, "failed to open keyring entry for delete");
            return Err(err.into());
        }
    };
    match entry.delete_credential() {
        Ok(()) => {
            // A logout's keyring half — worth seeing at default level,
            // same tier as `Db::forget_account`'s own info! for the other half.
            tracing::info!(account_id, ?storefront, "deleted session from keyring");
            Ok(())
        }
        Err(keyring::Error::NoEntry) => {
            tracing::debug!(account_id, ?storefront, "no session to delete in keyring");
            Ok(())
        }
        Err(err) => {
            tracing::error!(account_id, ?storefront, error = %err, "failed to delete session from keyring");
            Err(err.into())
        }
    }
}

/// A secret of a shadNet account, filed apart from the NPID that names it.
#[derive(Clone, Copy, Debug)]
#[non_exhaustive]
pub enum ShadnetSecret {
    Password,
    Token,
}

impl ShadnetSecret {
    const fn name(self) -> &'static str {
        match self {
            Self::Password => "password",
            Self::Token => "token",
        }
    }
}

fn shadnet_account(npid: &str, secret: ShadnetSecret) -> String {
    format!("shadnet/{npid}/{}", secret.name())
}

/// `None` when nothing was saved for this NPID. The caller checked `npid`.
#[inline]
pub fn load_shadnet(npid: &str, secret: ShadnetSecret) -> Result<Option<String>> {
    let result = read(&shadnet_account(npid, secret));
    match &result {
        Ok(found) => tracing::debug!(
            npid,
            ?secret,
            found = found.is_some(),
            "read shadNet secret from keyring"
        ),
        Err(err) => tracing::error!(npid, ?secret, error = %err, "failed to read shadNet secret from keyring"),
    }
    result
}

#[inline]
pub fn save_shadnet(npid: &str, secret: ShadnetSecret, value: &str) -> Result<()> {
    let result = Entry::new(SERVICE, &shadnet_account(npid, secret)).and_then(|entry| entry.set_password(value));
    match &result {
        Ok(()) => tracing::info!(npid, ?secret, "saved shadNet secret to keyring"),
        Err(err) => tracing::error!(npid, ?secret, error = %err, "failed to save shadNet secret to keyring"),
    }
    result.map_err(KeyringError::from)
}

/// Forgetting a secret that was never saved is a success, as for a session.
#[inline]
pub fn delete_shadnet(npid: &str, secret: ShadnetSecret) -> Result<()> {
    let result = Entry::new(SERVICE, &shadnet_account(npid, secret)).and_then(|entry| entry.delete_credential());
    match result {
        Ok(()) => {
            tracing::info!(npid, ?secret, "deleted shadNet secret from keyring");
            Ok(())
        }
        Err(keyring::Error::NoEntry) => Ok(()),
        Err(err) => {
            tracing::error!(npid, ?secret, error = %err, "failed to delete shadNet secret from keyring");
            Err(err.into())
        }
    }
}

/// Reads one keyring entry, mapping a missing entry to `None` rather than
/// an error — the ordinary "nobody logged in" case.
fn read(account: &str) -> Result<Option<String>> {
    let entry = match Entry::new(SERVICE, account) {
        Ok(entry) => entry,
        Err(err) => return Err(err.into()),
    };
    match entry.get_password() {
        Ok(secret) => Ok(Some(secret)),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(err) => Err(err.into()),
    }
}
