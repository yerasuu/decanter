// SPDX-License-Identifier: GPL-3.0-or-later
//! Whose key it is: `/v1/users/validate.json` (secondary, analysis section 1), the harmless call
//! that checks a key and says whether the account is Premium, which alone gets download links
//! through the API. The answer also echoes the key and the account's e-mail; neither is read.

use serde::Deserialize;

use crate::http::Client;
use crate::id::{api_url, display_text};
use crate::{Error, Result};

const CALL: &str = "the key check";
const MAX_BYTES: u64 = 64 << 10;
const MAX_NAME_CHARS: usize = 100;

#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Account {
    pub name: String,
    pub user_id: u64,
    pub is_premium: bool,
    pub is_supporter: bool,
}

#[derive(Deserialize)]
struct Raw {
    #[serde(default)]
    user_id: Option<u64>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    is_premium: Option<bool>,
    #[serde(default, rename = "is_premium?")]
    is_premium_legacy: Option<bool>,
    #[serde(default)]
    is_supporter: Option<bool>,
    #[serde(default, rename = "is_supporter?")]
    is_supporter_legacy: Option<bool>,
}

/// The account the client's key belongs to.
///
/// # Errors
///
/// [`Error::KeyRefused`] when Nexus does not take the key, as a request otherwise, and
/// [`Error::Malformed`] when the answer names no account.
#[inline]
pub async fn validate(client: &Client) -> Result<Account> {
    let url = api_url("/v1/users/validate.json").ok_or_else(|| Error::malformed(CALL, "its address does not parse"))?;
    let body = client.get_api(url, CALL, MAX_BYTES).await?;
    let raw: Raw = serde_json::from_slice(&body)
        .map_err(|err| Error::malformed(CALL, format!("line {}, column {}", err.line(), err.column())))?;
    let name = raw
        .name
        .as_deref()
        .map(|name| display_text(name, MAX_NAME_CHARS))
        .filter(|name| !name.is_empty())
        .ok_or_else(|| Error::malformed(CALL, "no account name"))?;
    let user_id = raw.user_id.ok_or_else(|| Error::malformed(CALL, "no user id"))?;
    // Whether Premium is spelt with or without the `?` is secondary; a missing flag is read as
    // free, which only hides the download button.
    let is_premium = raw.is_premium.or(raw.is_premium_legacy);
    if is_premium.is_none() {
        tracing::warn!("nexus key check carries no premium flag, treated as a free account");
    }
    let account = Account {
        name,
        user_id,
        is_premium: is_premium.unwrap_or_default(),
        is_supporter: raw.is_supporter.or(raw.is_supporter_legacy).unwrap_or_default(),
    };
    tracing::info!(
        user_id = account.user_id,
        is_premium = account.is_premium,
        is_supporter = account.is_supporter,
        "nexus api key checked"
    );
    Ok(account)
}
