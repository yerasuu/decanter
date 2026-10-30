//! Logging in to GOG, in-process.
//!
//! Replaces next-plugin-gog's `BeginLogin`/`CompleteLogin` RPCs. GOG's
//! OAuth flow has no device/loopback variant, so it goes: open
//! [`auth_url`] in a browser, log in, and GOG redirects to a page whose
//! URL carries a one-shot `code`. The caller hands that back to
//! [`log_in`], which trades it for a session and stores it — under the
//! account id GOG hands back, since that's the only thing this crate
//! knows about identity. Grouping the resulting account under a profile
//! is the caller's job (`decanter_core::Db::upsert_account`); this crate has
//! no notion of a profile at all.

use anyhow::Context as _;
use decanter_core::secrets::Secret;

use crate::api::client::{CLIENT_ID, Client, REDIRECT_URI};

/// Who a stored session belongs to.
#[derive(Debug, Clone)]
pub struct Account {
    pub account_id: String,
    pub display_name: String,
}

/// The page to send the user to. After logging in they land on a
/// `.../on_login_success?...&code=...` URL — that whole URL, or just the
/// code out of it, is what [`log_in`] wants.
pub fn auth_url() -> String {
    format!(
        "https://auth.gog.com/auth?client_id={CLIENT_ID}&redirect_uri={}&response_type=code&layout=client2",
        urlencode(REDIRECT_URI)
    )
}

/// Pulls the authorization code out of whatever the user pasted —
/// the full redirect URL or the bare code — so they don't have to hand-
/// edit a query string.
pub fn extract_code(pasted: &str) -> Option<String> {
    let pasted = pasted.trim();
    if pasted.is_empty() {
        return None;
    }
    let Some(query) = pasted.split_once('?').map(|(_, query)| query) else {
        // No query string: assume it's already just the code.
        return Some(pasted.to_string());
    };
    query
        .split('&')
        .filter_map(|pair| pair.split_once('='))
        .find(|(key, _)| *key == "code")
        .map(|(_, value)| value.to_string())
}

/// Trades an authorization code for a session and stores it.
///
/// No account id goes in: which account this is isn't known until GOG
/// answers, so the returned [`Account::account_id`] is what the session
/// ends up stored under, not something the caller supplies up front.
///
/// The code is single-use and short-lived — a failure here means starting
/// over from [`auth_url`], not retrying with the same code.
pub async fn log_in(pasted_code: &str) -> anyhow::Result<Account> {
    let code = crate::achv_try!(extract_code(pasted_code).context("no authorization code in that text"));
    // Never log the code itself, or the pasted URL it came from — it's a
    // single-use bearer credential, only whether one was found matters here.
    tracing::debug!("extracted OAuth authorization code from pasted login input");

    let client = crate::achv_try!(
        Client::from_login_code(&code)
            .await
            .map_err(|err| crate::session::failed("GOG did not accept the authorization code", err))
    );
    tracing::debug!("exchanged authorization code for a GOG session token");
    let user = crate::achv_try!(
        client
            .user()
            .await
            .map_err(|err| crate::session::failed("GOG session check failed", err))
    );
    // Read the token back out: fetching user data may have refreshed it.
    let credentials =
        crate::achv_try!(serde_json::to_string(&client.token().await).context("serializing the GOG session"));
    // Under the account's lock so that a call still running on a previous
    // session finishes first, instead of storing its token over this one.
    {
        let lock = crate::session_lock(&user.id);
        let _guard = lock.lock().await;
        crate::achv_try!(crate::write_session(&user.id, Secret::from(credentials)).await);
    }
    tracing::info!(account_id = %user.id, "GOG login succeeded");

    Ok(Account {
        account_id: user.id,
        display_name: user.username,
    })
}

/// Drops the stored session. Who the account belonged to isn't recorded
/// anywhere else — [`crate::check_session`] answers that by asking GOG — so forgetting
/// the credentials is the whole of logging out.
pub async fn log_out(account_id: &str) -> anyhow::Result<()> {
    crate::clear_session(account_id).await?;
    tracing::info!(account_id, "logged out of GOG account");
    Ok(())
}

/// Percent-encodes the redirect URI for the auth URL's query string. Small
/// enough to hand-roll rather than take a dependency for one constant.
fn urlencode(value: &str) -> String {
    value
        .chars()
        .map(|c| match c {
            'A'..='Z' | 'a'..='z' | '0'..='9' | '-' | '_' | '.' | '~' => c.to_string(),
            _ => format!("%{:02X}", c as u32),
        })
        .collect()
}
