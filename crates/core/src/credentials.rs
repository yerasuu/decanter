//! Storefront sessions, in the OS keyring.
//!
//! One entry per `(profile, storefront)`, holding whatever that
//! storefront's client needs to resume a session — for GOG, a serialized
//! `gog::token::Token`. Never on disk in the clear: these are live
//! credentials to someone's game library and payment account.
//!
//! Blocking on purpose. The Secret Service is a D-Bus round trip, called
//! a handful of times per session; async callers wrap these in
//! `spawn_blocking` rather than this crate taking on a runtime.

#![expect(
    clippy::std_instead_of_core,
    reason = "desktop app, no no_std target exists to justify core:: here"
)]
// Only fires on some compilation targets (lib vs tests), so `expect` can't
// be used here without an "unfulfilled expectation" warning on the other.
#![allow(
    clippy::self_named_module_files,
    reason = "convention here is `<name>.rs` plus a sibling `<name>/tests.rs`, see CLAUDE.md"
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

/// Keyring service name for achv's own entries.
const SERVICE: &str = "dev.achv.Achv";

/// The service bottles-next stores its sessions under. Read once, on a
/// miss, so a user who logged in through next-ui before achv owned
/// this doesn't get silently logged out — see [`load`].
const LEGACY_SERVICE: &str = "com.usebottles.Bottles";

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum KeyringError {
    #[error("keyring: {0}")]
    Keyring(#[from] keyring::Error),
}

pub type Result<T> = StdResult<T, KeyringError>;

/// The account half of a keyring entry. Shared with the legacy service, so
/// an entry written by bottles-next is found at the same account name.
fn account(profile_id: &str, storefront: Storefront) -> String {
    format!("{profile_id}/{}", storefront.as_str_name())
}

/// `None` means nobody is logged in for this profile and storefront — the
/// ordinary case, not an error.
///
/// A miss falls back to bottles-next's own entry and, on a hit, copies it
/// into achv's before returning it, so the fallback runs at most once
/// per profile. A failure to copy is ignored: the session was still read,
/// and the next call just falls back again.
#[inline]
pub fn load(profile_id: &str, storefront: Storefront) -> Result<Option<String>> {
    let account = account(profile_id, storefront);
    let primary = match read(SERVICE, &account) {
        Ok(secret) => secret,
        Err(err) => return Err(err),
    };
    if let Some(secret) = primary {
        return Ok(Some(secret));
    }
    let legacy = match read(LEGACY_SERVICE, &account) {
        Ok(secret) => secret,
        Err(err) => return Err(err),
    };
    let Some(inherited) = legacy else {
        return Ok(None);
    };
    if let Err(err) = save(profile_id, storefront, &inherited) {
        tracing::warn!("could not adopt the bottles-next session for {account}: {err}");
    }
    Ok(Some(inherited))
}

#[inline]
pub fn save(profile_id: &str, storefront: Storefront, secret: &str) -> Result<()> {
    let entry = match Entry::new(SERVICE, &account(profile_id, storefront)) {
        Ok(entry) => entry,
        Err(err) => return Err(err.into()),
    };
    match entry.set_password(secret) {
        Ok(()) => Ok(()),
        Err(err) => Err(err.into()),
    }
}

/// Deleting a session nobody has is a success: the caller wanted it gone,
/// and it is.
#[inline]
pub fn delete(profile_id: &str, storefront: Storefront) -> Result<()> {
    let account = account(profile_id, storefront);
    // The legacy entry goes too — leaving it would have [`load`] adopt it
    // straight back on the next call and undo the logout.
    for service in [SERVICE, LEGACY_SERVICE] {
        let entry = match Entry::new(service, &account) {
            Ok(entry) => entry,
            Err(err) => return Err(err.into()),
        };
        match entry.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => {}
            Err(err) => return Err(err.into()),
        }
    }
    Ok(())
}

/// Reads one keyring entry, mapping a missing entry to `None` rather than
/// an error — the ordinary "nobody logged in" case.
fn read(service: &str, account: &str) -> Result<Option<String>> {
    let entry = match Entry::new(service, account) {
        Ok(entry) => entry,
        Err(err) => return Err(err.into()),
    };
    match entry.get_password() {
        Ok(secret) => Ok(Some(secret)),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(err) => Err(err.into()),
    }
}

#[cfg(test)]
mod tests;
