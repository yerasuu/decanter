//! Logging in to GOG, in-process.
//!
//! Replaces next-plugin-gog's `BeginLogin`/`CompleteLogin` RPCs. GOG's
//! OAuth flow has no device/loopback variant, so it goes: open
//! [`auth_url`] in a browser, log in, and GOG redirects to a page whose
//! URL carries a one-shot `code`. The caller hands that back to
//! [`log_in`], which trades it for a session and stores it — under the
//! account id GOG hands back, since that's the only thing this crate
//! knows about identity. Grouping the resulting account under a profile
//! is the caller's job (`achv_core::Db::upsert_account`); this crate has
//! no notion of a profile at all.

use anyhow::Context as _;

/// GOG Galaxy's public client ID, used by every third-party GOG client
/// (Heroic, Minigalaxy, Lutris, ...) since GOG has no per-application
/// client registration for this flow.
const GOG_CLIENT_ID: &str = "46899977096215655";

/// Where GOG redirects after a successful login, carrying the
/// authorization `code` as a query parameter. Not an endpoint anyone
/// controls — it just renders a page, and the code is read off its URL.
const GOG_REDIRECT_URI: &str = "https://embed.gog.com/on_login_success?origin=client";

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
        "https://auth.gog.com/auth?client_id={GOG_CLIENT_ID}&redirect_uri={}&response_type=code&layout=client2",
        urlencode(GOG_REDIRECT_URI)
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

    let (token, user) = crate::achv_try!(crate::achv_try!(
        tokio::task::spawn_blocking(move || -> anyhow::Result<_> {
            let token = crate::achv_try!(
                gog::token::Token::from_login_code(&code)
                    .map_err(|err| anyhow::anyhow!("GOG rejected the authorization code: {err}"))
            );
            tracing::debug!("exchanged authorization code for a GOG session token");
            let client = gog::Gog::new(token);
            let user = crate::achv_try!(
                client
                    .get_user_data()
                    .map_err(|err| anyhow::anyhow!("GOG session check failed: {err}"))
            );
            // Read the token back out: fetching user data may have refreshed it.
            Ok((client.token.borrow().clone(), user))
        })
        .await
        .context("GOG worker task panicked")
    ));

    let credentials = crate::achv_try!(serde_json::to_string(&token).context("serializing the GOG session"));
    crate::achv_try!(crate::write_session(&user.user_id, credentials).await);
    tracing::info!(account_id = %user.user_id, "GOG login succeeded");

    Ok(Account {
        account_id: user.user_id.clone(),
        display_name: user.username.clone(),
    })
}

/// Verifies the stored session still works, refreshing it in passing.
/// `None` means nobody is logged in on this account; an error means there
/// is a session but GOG wouldn't accept it.
pub async fn session(account_id: &str) -> anyhow::Result<Option<Account>> {
    if crate::achv_try!(crate::read_session(account_id).await).is_none() {
        tracing::debug!(account_id, "no stored GOG session for this account");
        return Ok(None);
    }

    let user = crate::achv_try!(
        crate::with_session(account_id, |client| {
            client
                .get_user_data()
                .map_err(|err| anyhow::anyhow!("GOG session is no longer valid: {err}"))
        })
        .await
    );
    tracing::debug!(account_id, "verified GOG session is still valid");

    Ok(Some(Account {
        account_id: user.user_id,
        display_name: user.username,
    }))
}

/// Drops the stored session. Who the account belonged to isn't recorded
/// anywhere else — [`session`] answers that by asking GOG — so forgetting
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

#[cfg(test)]
mod tests;
