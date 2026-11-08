// SPDX-License-Identifier: GPL-3.0-or-later
//! Logging in to Epic: the browser step and the token grant it ends in (spec, section 3.1).
//!
//! [`login_url`] is Epic's own login page, told to land on a redirect page of the launcher's
//! client. Once the user logs in there, that page shows a small JSON document whose
//! `authorizationCode` is a one-shot code; pasted back into [`complete_login`], whole or as the
//! bare value, it is traded for the account's session. Nothing has to be remembered between the
//! two steps, so a failed login is retried from the same page.

use anyhow::Context as _;
use decanter_core::secrets::Secret;
use serde::Deserialize;
use url::form_urlencoded::Serializer;

use crate::identity::CLIENT_ID;
use crate::session::{self, Account, Session};
use crate::{http, token};

const LOGIN_PAGE: &str = "https://www.epicgames.com/id/login";
const REDIRECT_PAGE: &str = "https://www.epicgames.com/id/api/redirect";
/// Epic publishes no length for its authorization codes (UNVERIFIED); anything near this is not
/// one but the wrong part of the pasted text.
const CODE_MAX_LEN: usize = 256;

/// The redirect page's JSON. Its other members are not read: `authorizationCode` is the only one
/// the login uses, and `null` when the browser was not logged in.
#[derive(Deserialize)]
struct RedirectPage {
    #[serde(rename = "authorizationCode", default)]
    code: Option<String>,
}

/// The login page to open in a browser.
#[inline]
#[must_use]
pub fn login_url() -> String {
    let redirect = format!(
        "{REDIRECT_PAGE}?{}",
        Serializer::new(String::new())
            .append_pair("clientId", CLIENT_ID)
            .append_pair("responseType", "code")
            .finish()
    );
    let query = Serializer::new(String::new())
        .append_pair("redirectUrl", &redirect)
        .finish();
    format!("{LOGIN_PAGE}?{query}")
}

/// Trades the code in what the user pasted for the account's session and stores it, replacing
/// whatever was stored for that account.
///
/// The code is single use and short lived (UNVERIFIED, spec Q18): a failure means loading the
/// redirect page again for a new one.
///
/// # Errors
///
/// No code in the pasted text, Epic refusing the code, asking for an action on its website or not
/// answering, or a local problem with the keyring or the HTTP client.
#[inline]
pub async fn complete_login(pasted: &str) -> anyhow::Result<Account> {
    let code = extract_code(pasted).context("no Epic authorization code in that text")?;
    // Neither the code nor the pasted text is logged: the code is a single-use credential.
    tracing::debug!("extracted the authorization code from the pasted Epic redirect page");

    let http = http::client()?;
    let grant = token::redeem_code(&http, &code)
        .await
        .map_err(|err| session::failed("Epic did not accept the authorization code", err))?;
    let fresh = Session::granted(grant);
    session::replace(&fresh).await?;
    let account = fresh.account();
    tracing::info!(account_id = %account.account_id, "epic login succeeded");
    Ok(account)
}

/// The authorization code in what the user pasted: the redirect page's JSON, or the bare code,
/// with or without the double quotes it is shown in. `None` when neither holds a code that can
/// be one.
fn extract_code(pasted: &str) -> Option<Secret> {
    let text = pasted.trim();
    let value = if text.starts_with('{') {
        serde_json::from_str::<RedirectPage>(text).ok()?.code?
    } else {
        text.strip_prefix('"')
            .and_then(|quoted| quoted.strip_suffix('"'))
            .unwrap_or(text)
            .to_owned()
    };
    let code = Secret::from(value);
    is_code(&code).then_some(code)
}

/// The code goes back as a form value, so anything beyond letters, digits, `-` and `_` means the
/// wrong part of the pasted text was taken.
fn is_code(code: &Secret) -> bool {
    let value = code.expose();
    (1..=CODE_MAX_LEN).contains(&value.len())
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}
