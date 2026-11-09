//! decanter's side of mod.io (`decanter_modio`): the user's own API key in the keyring (decanter
//! ships none, decision #1629) and the line each failure is shown as, shared by Ajustes and the
//! Mods tab.

use decanter_core::secrets::{self, Secret, SecretKey};
use decanter_modio::{Client, Error};

use crate::i18n::Text;

/// The saved key, `None` when there is none. Blocking: a keyring round trip.
pub fn api_key() -> Result<Option<Secret>, Text> {
    secrets::store().get(&SecretKey::MODIO).map_err(|err| {
        tracing::warn!(%err, "could not read the modio key from the keyring");
        Text::new("mods.modio.errors.keyring").arg("error", err)
    })
}

/// A client on the saved key. Blocking, like [`api_key`].
pub fn client() -> Result<Client, Text> {
    let key = api_key()?.ok_or_else(|| failure(&Error::NoKey))?;
    Client::new(key).map_err(|err| failure(&err))
}

/// Saves `key` once it has a key's shape. Blocking.
pub fn set_api_key(key: String) -> Result<(), Text> {
    if !decanter_modio::is_key_shaped(&key) {
        return Err(Text::new("mods.modio.key.bad_shape"));
    }
    secrets::store()
        .put(&SecretKey::MODIO, &Secret::from(key))
        .inspect(|()| tracing::info!("modio key saved"))
        .map_err(|err| Text::new("mods.modio.errors.keyring").arg("error", err))
}

/// Forgets the key. Blocking.
pub fn forget_api_key() -> Result<(), Text> {
    secrets::store()
        .delete(&SecretKey::MODIO)
        .inspect(|()| tracing::info!("modio key forgotten"))
        .map_err(|err| Text::new("mods.modio.errors.keyring").arg("error", err))
}

/// The line a failure is shown as: its own for what the user can act on, mod.io's reason with a
/// "try later" for what may pass.
pub fn failure(err: &Error) -> Text {
    match err {
        Error::NoKey => Text::new("mods.modio.errors.no_key"),
        Error::KeyRejected { .. } => Text::new("mods.modio.errors.key_rejected"),
        Error::TermsRequired { .. } => Text::new("mods.modio.errors.terms"),
        Error::NoThirdPartyAccess => Text::new("mods.modio.errors.no_third_party"),
        Error::NeedsLogin => Text::new("mods.modio.errors.needs_login"),
        Error::RateLimited { seconds, .. } => Text::new("mods.modio.errors.rate_limited").arg("seconds", seconds),
        Error::Malicious => Text::new("mods.modio.errors.malicious"),
        Error::HashMismatch => Text::new("mods.modio.errors.hash"),
        Error::LinkExpired => Text::new("mods.modio.errors.expired"),
        Error::NoFile => Text::new("mods.modio.errors.no_file"),
        Error::Cancelled => Text::new("mods.modio.cancelled"),
        _ if err.is_retryable() => Text::new("mods.modio.failed_retry").arg("reason", err),
        _ => Text::new("mods.modio.failed").arg("reason", err),
    }
}
