//! The OS keyring through `keyring-core` and the store crate of each platform
//! ([keyring-core](https://docs.rs/keyring-core/1.0.0/keyring_core/)).

use std::sync::Arc;

use keyring_core::{CredentialStore, Entry, Error};

use super::Secret;
use super::store::{Backend, BackendKind, Failure};

/// The service every decanter entry is filed under.
const SERVICE: &str = "dev.decanter.Decanter";

pub(super) struct Native {
    store: Arc<CredentialStore>,
}

/// Builds this platform's store. On Linux that opens the D-Bus session and the Secret Service,
/// which is what fails when there is no session bus or no provider.
pub(super) fn connect() -> Result<Native, String> {
    #[cfg(target_os = "linux")]
    let store = zbus_secret_service_keyring_store::Store::new();
    #[cfg(windows)]
    let store = windows_native_keyring_store::Store::new();
    #[cfg(target_os = "macos")]
    let store = apple_native_keyring_store::keychain::Store::new();
    #[cfg(not(any(target_os = "linux", windows, target_os = "macos")))]
    let store: Result<Arc<CredentialStore>, Error> = Err(Error::NotSupportedByStore(
        "no keyring backend for this platform".to_owned(),
    ));
    match store {
        Ok(store) => Ok(Native { store }),
        Err(err) => Err(failure(err).into_reason()),
    }
}

impl Native {
    fn entry(&self, name: &str) -> Result<Entry, Failure> {
        self.store.build(SERVICE, name, None).map_err(failure)
    }
}

impl Backend for Native {
    fn kind(&self) -> BackendKind {
        BackendKind::Native
    }

    fn get(&self, entry: &str) -> Result<Option<Secret>, Failure> {
        match self.entry(entry)?.get_password() {
            Ok(value) => Ok(Some(Secret::from(value))),
            Err(Error::NoEntry) => Ok(None),
            Err(err) => Err(failure(err)),
        }
    }

    fn put(&self, entry: &str, secret: &Secret) -> Result<(), Failure> {
        self.entry(entry)?.set_password(secret.expose()).map_err(failure)
    }

    fn delete(&self, entry: &str) -> Result<(), Failure> {
        match self.entry(entry)?.delete_credential() {
            Ok(()) | Err(Error::NoEntry) => Ok(()),
            Err(err) => Err(failure(err)),
        }
    }
}

/// Keeps the platform's message and drops whatever bytes the error carries: `BadEncoding` and
/// `BadDataFormat` hold the stored value itself, and the latter's `Display` prints its cause with
/// `Debug`.
fn failure(err: Error) -> Failure {
    match err {
        Error::PlatformFailure(cause) => Failure::Unavailable(cause.to_string()),
        Error::NoDefaultStore => Failure::Unavailable("no default store".to_owned()),
        Error::NoStorageAccess(cause) => Failure::Locked(cause.to_string()),
        Error::TooLong(attribute, limit) => Failure::TooLarge(format!("{attribute} over the limit of {limit}")),
        Error::BadEncoding(_) => Failure::Corrupt("not UTF-8"),
        Error::BadDataFormat(..) => Failure::Corrupt("not in the store's format"),
        Error::BadStoreFormat(reason) | Error::NotSupportedByStore(reason) => Failure::Refused(reason),
        Error::Invalid(attribute, reason) => Failure::Refused(format!("{attribute}: {reason}")),
        Error::Ambiguous(entries) => Failure::Refused(format!("{} entries match", entries.len())),
        Error::NoEntry => Failure::Refused("no such entry".to_owned()),
        // keyring_core::Error is non_exhaustive; a variant added later says nothing this
        // mapping could keep without reading its fields.
        _ => Failure::Refused("unrecognized keyring error".to_owned()),
    }
}
