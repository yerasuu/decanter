// SPDX-License-Identifier: GPL-3.0-or-later
//! The user's personal API key in the OS keyring, the only place it is kept: never in the library,
//! a settings file, `mod.toml` or a log. Each call is a keyring round trip, so callers run it off
//! the UI thread.

use decanter_core::secrets::{self, Secret, SecretKey};

use crate::{Error, Result};

/// # Errors
///
/// [`Error::Keyring`] when the keyring can't be read.
#[inline]
pub fn load_key() -> Result<Option<Secret>> {
    secrets::store().get(&SecretKey::NEXUS).map_err(Error::Keyring)
}

/// Saves `key`, trimmed, over any saved before.
///
/// # Errors
///
/// [`Error::NoKey`] for an empty key, [`Error::Keyring`] when the keyring refuses it.
#[inline]
pub fn save_key(key: &Secret) -> Result<()> {
    let trimmed = key.expose().trim();
    if trimmed.is_empty() {
        return Err(Error::NoKey);
    }
    secrets::store()
        .put(&SecretKey::NEXUS, &Secret::from(trimmed.to_owned()))
        .map_err(Error::Keyring)?;
    tracing::info!("nexus api key saved");
    Ok(())
}

/// # Errors
///
/// [`Error::Keyring`] when the keyring can't remove it.
#[inline]
pub fn forget_key() -> Result<()> {
    secrets::store().delete(&SecretKey::NEXUS).map_err(Error::Keyring)?;
    tracing::info!("nexus api key forgotten");
    Ok(())
}
