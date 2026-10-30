//! The OS keyring through `keyring-core` and the store crate of each platform
//! ([keyring-core](https://docs.rs/keyring-core/1.0.0/keyring_core/)).

use std::collections::HashMap;
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
    match open() {
        Ok(store) => Ok(Native { store }),
        Err(err) => Err(failure(err).reason().to_owned()),
    }
}

fn open() -> Result<Arc<CredentialStore>, Error> {
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
    store.map(|store| -> Arc<CredentialStore> { store })
}

/// The entry names under decanter's service, by the store's own search. A free function since the
/// private `Backend` trait does not search; it opens its own connection, which a settings screen
/// asking now and then can afford. A store that does not search answers with `Refused`.
pub(super) fn search_names() -> Result<Vec<String>, Failure> {
    let store = open().map_err(failure)?;
    #[cfg(windows)]
    let spec = HashMap::from([("pattern", r"\.dev\.decanter\.Decanter$")]);
    #[cfg(not(windows))]
    let spec = HashMap::from([("service", SERVICE)]);
    let found = store.search(&spec).map_err(failure)?;
    Ok(found
        .iter()
        .filter_map(Entry::get_specifiers)
        .filter_map(|(service, user)| entry_name(&service, user))
        .collect())
}

/// The entry name in a search hit's specifiers. The Windows store splits a target name `user.service`
/// at its last dot, which cuts the dotted service in two: the service it reports is `Decanter` and
/// the user keeps `.dev.decanter` on its end.
fn entry_name(service: &str, user: String) -> Option<String> {
    if service == SERVICE {
        return Some(user);
    }
    let (prefix, last) = SERVICE.rsplit_once('.')?;
    if service == last {
        user.strip_suffix(&format!(".{prefix}")).map(str::to_owned)
    } else {
        None
    }
}

impl Native {
    fn entry(&self, name: &str) -> Result<Entry, Failure> {
        self.store.build(SERVICE, name, None).map_err(failure)
    }

    /// An entry to write through. On Windows it carries `Local` persistence, which the store
    /// applies when the secret is written: its default, `Enterprise`, roams with a domain
    /// profile, and decanter's secrets stay on this machine (decision #1299).
    fn writer(&self, name: &str) -> Result<Entry, Failure> {
        #[cfg(windows)]
        {
            let modifiers = HashMap::from([("persistence", "Local")]);
            self.store.build(SERVICE, name, Some(&modifiers)).map_err(failure)
        }
        #[cfg(not(windows))]
        self.entry(name)
    }

    /// Writes an entry saved with `Enterprise` persistence again as `Local`, and counts it done
    /// only once it reads back the same; until then it stays as it was, and the next read tries
    /// again.
    #[cfg(windows)]
    fn keep_local(&self, name: &str, secret: &Secret) {
        let persistence = self
            .entry(name)
            .and_then(|entry| entry.get_attributes().map_err(failure));
        match persistence {
            Ok(attributes) if attributes.get("persistence").is_some_and(|value| value == "Enterprise") => {}
            Ok(_) => return,
            Err(failure) => {
                tracing::warn!(
                    entry = name,
                    reason = failure.reason(),
                    "keyring entry persistence unreadable"
                );
                return;
            }
        }
        let rewritten = self
            .put(name, secret)
            .and_then(|()| self.entry(name)?.get_password().map(Secret::from).map_err(failure));
        match rewritten {
            Ok(back) if back.expose() == secret.expose() => {
                tracing::info!(entry = name, "keyring entry moved to local persistence");
            }
            Ok(_) => tracing::warn!(
                entry = name,
                "keyring entry read back otherwise after the move to local persistence"
            ),
            Err(failure) => tracing::warn!(
                entry = name,
                reason = failure.reason(),
                "keyring entry not moved to local persistence, retried on a later read"
            ),
        }
    }
}

impl Backend for Native {
    fn kind(&self) -> BackendKind {
        BackendKind::Native
    }

    fn get(&self, entry: &str) -> Result<Option<Secret>, Failure> {
        let found = match self.entry(entry)?.get_password() {
            Ok(value) => Secret::from(value),
            Err(Error::NoEntry) => return Ok(None),
            Err(err) => return Err(failure(err)),
        };
        #[cfg(windows)]
        self.keep_local(entry, &found);
        Ok(Some(found))
    }

    fn put(&self, entry: &str, secret: &Secret) -> Result<(), Failure> {
        self.writer(entry)?.set_password(secret.expose()).map_err(failure)
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
