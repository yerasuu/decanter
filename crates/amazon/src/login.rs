// SPDX-License-Identifier: GPL-3.0-or-later
//! Logging in to Amazon: the browser step and the device registration it ends in.
//!
//! A [`PendingLogin`] makes the PKCE verifier and the device identity, and its
//! [`PendingLogin::sign_in_url`] is the page to open. The user logs in there and lands on an
//! amazon.com URL whose query carries a one-shot authorization code; pasted back into
//! [`complete_login`], it registers decanter as a launcher device on that account and stores the
//! session (analysis, section 2.1, UNVERIFIED). The sign-in page's parameters are the ones the
//! analysis lists plus the OpenID fields nile sends, checked against nile 1.2.0's own URL (kb
//! #1607), which carries the challenge as `openid.oa2.code_challenge`.

use core::fmt::Write as _;

use anyhow::Context as _;
use decanter_core::secrets::Secret;
use url::Url;
use url::form_urlencoded::Serializer;

use crate::device::{self, Device};
use crate::identity::{ASSOC_HANDLE, CODE_PARAM, DEVICE_TYPE, MARKETPLACE_ID, RETURN_HOSTS, RETURN_TO, SIGN_IN_URL};
use crate::session::{self, Account, Session};
use crate::{http, pkce};

/// Amazon publishes no length for its authorization codes (UNVERIFIED); anything near this is
/// not one but the wrong part of the pasted text.
const CODE_MAX_LEN: usize = 1024;

const OPENID_NS: &str = "http://specs.openid.net/auth/2.0";
const OPENID_IDENTIFIER_SELECT: &str = "http://specs.openid.net/auth/2.0/identifier_select";
const OA2_NS: &str = "http://www.amazon.com/ap/ext/oauth/2";

/// One login in progress: what the sign-in page was opened with and the registration must send
/// again. Held in memory only, by whoever opened the page, until the redirect is pasted back.
#[derive(Debug)]
pub struct PendingLogin {
    verifier: Secret,
    device_serial: String,
    client_id: String,
}

impl PendingLogin {
    /// A new verifier and a new device identity.
    ///
    /// The serial has the shape nile gives it, 32 upper-case hex digits (analysis, section 2.1),
    /// but comes from a random UUID instead of a time-and-MAC one, so that it does not carry the
    /// machine's network address to Amazon. The client id is the lower-case hex of
    /// `<serial>#<device type>`, the case Python's `bytes.hex` writes (INFER).
    ///
    /// # Errors
    ///
    /// The system's random source failing.
    #[inline]
    pub fn new() -> anyhow::Result<Self> {
        let verifier = pkce::verifier().context("making the PKCE verifier")?;
        let device_serial = uuid::Uuid::new_v4().simple().to_string().to_ascii_uppercase();
        let client_id =
            format!("{device_serial}#{DEVICE_TYPE}")
                .bytes()
                .fold(String::with_capacity(64), |mut hex, byte| {
                    let _infallible = write!(hex, "{byte:02x}");
                    hex
                });
        Ok(Self {
            verifier,
            device_serial,
            client_id,
        })
    }

    /// The sign-in page to open in a browser.
    #[inline]
    #[must_use]
    pub fn sign_in_url(&self) -> String {
        let client_id = format!("device:{}", self.client_id);
        let query = Serializer::new(String::new())
            .append_pair("openid.ns", OPENID_NS)
            .append_pair("openid.claimed_id", OPENID_IDENTIFIER_SELECT)
            .append_pair("openid.identity", OPENID_IDENTIFIER_SELECT)
            .append_pair("openid.mode", "checkid_setup")
            .append_pair("openid.ns.oa2", OA2_NS)
            .append_pair("language", "en_US")
            .append_pair("openid.oa2.scope", "device_auth_access")
            .append_pair("openid.oa2.response_type", "code")
            .append_pair("openid.oa2.code_challenge_method", "S256")
            .append_pair("openid.oa2.code_challenge", &pkce::challenge(&self.verifier))
            .append_pair("openid.oa2.client_id", &client_id)
            .append_pair("openid.assoc_handle", ASSOC_HANDLE)
            .append_pair("pageId", ASSOC_HANDLE)
            .append_pair("marketPlaceId", MARKETPLACE_ID)
            .append_pair("openid.return_to", RETURN_TO)
            .append_pair("openid.pape.max_auth_age", "0")
            .finish();
        format!("{SIGN_IN_URL}?{query}")
    }
}

/// The authorization code in what the user pasted: the whole redirect URL, which must be an
/// HTTPS amazon.com page, or the bare code. `None` when neither holds a code that can be one.
#[inline]
#[must_use]
pub fn extract_code(pasted: &str) -> Option<Secret> {
    let text = pasted.trim();
    let code = if text.contains("://") {
        let url = Url::parse(text).ok()?;
        let is_return = url.scheme() == "https" && url.host_str().is_some_and(|host| RETURN_HOSTS.contains(&host));
        if !is_return {
            return None;
        }
        url.query_pairs()
            .find_map(|(key, value)| (key == CODE_PARAM).then(|| Secret::from(value.into_owned())))?
    } else {
        Secret::from(text.to_owned())
    };
    is_code(&code).then_some(code)
}

/// Registers the device with the code of the pasted redirect and stores its session under the
/// account Amazon answers for.
///
/// The code is single use and short lived, and the pending login is spent with it: a failure
/// means starting over from a new [`PendingLogin`]. A session already stored for the account is
/// deregistered first, so that logging in again does not leave the old device on the account.
///
/// # Errors
///
/// No code in the pasted text, Amazon refusing the code or not answering, or a local problem
/// with the keyring or the HTTP client.
#[inline]
pub async fn complete_login(pending: PendingLogin, pasted: &str) -> anyhow::Result<Account> {
    let code = extract_code(pasted).context("no Amazon authorization code in that text")?;
    // Neither the code nor the pasted URL is logged: the code is a single-use credential.
    tracing::debug!("extracted the authorization code from the pasted Amazon redirect");

    let http = http::client()?;
    let device = Device {
        serial: &pending.device_serial,
        client_id: &pending.client_id,
    };
    let registration = device::register(&http, &code, &pending.verifier, &device)
        .await
        .map_err(|err| session::failed("Amazon did not register the device", err))?;
    let account = Account {
        account_id: registration.account_id.clone(),
        display_name: registration.display_name.clone(),
    };
    let fresh = Session::registered(registration, pending.device_serial, pending.client_id);
    session::replace(&http, &account.account_id, &fresh).await?;
    tracing::info!(account_id = %account.account_id, "amazon login succeeded");
    Ok(account)
}

/// The code goes back into a JSON body, so anything beyond the URL-unreserved characters means
/// the wrong part of the pasted text was taken.
fn is_code(code: &Secret) -> bool {
    let value = code.expose();
    (1..=CODE_MAX_LEN).contains(&value.len())
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~'))
}
