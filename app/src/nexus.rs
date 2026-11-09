//! decanter's side of Nexus Mods (`decanter_nexus`) that Ajustes and the Mods tab share: a client
//! built from the user's saved key and the lines a failure or an account shows as.

use std::path::PathBuf;

use decanter_nexus::{Account, Client, Error};
use rust_i18n::t;

use crate::i18n::Text;

/// A client with the saved key, read from the keyring off the UI thread.
///
/// # Errors
///
/// [`Error::NoKey`] with no key saved, [`Error::Keyring`] when the keyring can't be read.
pub async fn client() -> decanter_nexus::Result<Client> {
    let key = tokio::task::spawn_blocking(decanter_nexus::load_key)
        .await
        .map_err(|err| Error::Io {
            path: PathBuf::from("keyring"),
            source: std::io::Error::other(err),
        })??;
    Client::new(key.ok_or(Error::NoKey)?)
}

/// A failure's line; one Nexus may answer differently later says so.
pub fn failure_text(err: &Error) -> Text {
    match err {
        Error::NoKey => Text::new("mods.nexus.no_key"),
        Error::KeyRefused => Text::new("mods.nexus.key_refused"),
        Error::PremiumOnly => Text::new("mods.nexus.premium_only"),
        Error::RateLimited { resets } => Text::new("mods.nexus.rate_limited").arg("time", resets),
        Error::Cancelled => Text::new("mods.nexus.cancelled"),
        _ if err.is_retryable() => Text::new("mods.nexus.failed_retry").arg("reason", err),
        _ => Text::new("mods.nexus.failed").arg("reason", err),
    }
}

pub fn account_line(account: &Account) -> String {
    if account.is_premium {
        t!("mods.nexus.account_premium", name = account.name).into_owned()
    } else {
        t!("mods.nexus.account_free", name = account.name).into_owned()
    }
}
