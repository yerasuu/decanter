// SPDX-License-Identifier: GPL-3.0-or-later
//! The two calls of Epic's account service a session needs: the token endpoint, for the first
//! grant and every renewal, and verify, to reuse an access token without spending the refresh
//! token (spec, sections 3.2 and 3.3).
//!
//! Every grant also sends `token_type=eg1`, as the launcher does; what Epic answers without it is
//! UNVERIFIED (spec, Q4). The answer is read for the members the spec lists and nothing else.

use decanter_core::secrets::{Secret, SecretKey};
use reqwest::Method;
use serde::de::IgnoredAny;
use serde::{Deserialize, Deserializer};

use crate::error::CallError;
use crate::hosts::Host;
use crate::http::{self, Call, Kind};
use crate::identity::{CLIENT_ID, CLIENT_SECRET};

const TOKEN_PATH: &str = "/account/api/oauth/token";
const VERIFY_PATH: &str = "/account/api/oauth/verify";

const CODE_GRANT: Call = Call {
    name: "authorization code grant",
    host: Host::Accounts,
    kind: Kind::Grant,
    max_body: 64 << 10,
};
const REFRESH: Call = Call {
    name: "token refresh",
    host: Host::Accounts,
    kind: Kind::Grant,
    max_body: 64 << 10,
};
const VERIFY: Call = Call {
    name: "token verify",
    host: Host::Accounts,
    kind: Kind::Bearer,
    max_body: 64 << 10,
};

/// Two tokens and the rest of a session must fit under the keyring's cap on a stored session.
const TOKEN_MAX_LEN: usize = 6 << 10;
/// A month: an access token lasts hours (UNVERIFIED, spec 3.5 item 7), and one claiming longer is
/// not one decanter should trust to stay valid unrenewed.
const MAX_EXPIRES_IN: u64 = 30 * 24 * 3600;
const DISPLAY_NAME_MAX_CHARS: usize = 256;

/// One token answer: the whole record a session is replaced with, the refresh token included,
/// since Epic may rotate it on every use (spec, 3.5 item 5).
pub(crate) struct Grant {
    pub account_id: String,
    pub display_name: String,
    pub access_token: Secret,
    pub refresh_token: Secret,
    pub expires_in: u64,
}

#[derive(Deserialize)]
struct TokenAnswer {
    #[serde(deserialize_with = "secret")]
    access_token: Secret,
    #[serde(deserialize_with = "secret")]
    refresh_token: Secret,
    #[serde(default)]
    expires_in: Option<serde_json::Number>,
    #[serde(default)]
    expires_at: Option<String>,
    account_id: String,
    #[serde(rename = "displayName", default)]
    display_name: Option<String>,
}

/// verify marks a refused token with `errorMessage`, not with the status (spec, 3.3).
#[derive(Deserialize)]
struct VerifyAnswer {
    #[serde(rename = "errorMessage", default)]
    error_message: Option<IgnoredAny>,
    #[serde(rename = "errorCode", default)]
    error_code: Option<String>,
    #[serde(default)]
    account_id: Option<String>,
}

/// Trades the one-shot authorization code of the browser login for a session.
pub(crate) async fn redeem_code(http: &reqwest::Client, code: &Secret) -> Result<Grant, CallError> {
    grant(
        http,
        &CODE_GRANT,
        &[("grant_type", "authorization_code"), ("code", code.expose())],
    )
    .await
}

/// Renews a session with its refresh token; the answer carries the refresh token to keep next.
pub(crate) async fn refresh(http: &reqwest::Client, refresh_token: &Secret) -> Result<Grant, CallError> {
    grant(
        http,
        &REFRESH,
        &[
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token.expose()),
        ],
    )
    .await
}

/// Whether Epic still takes `access_token` as `account_id`'s.
pub(crate) async fn verify(http: &reqwest::Client, access_token: &Secret, account_id: &str) -> Result<(), CallError> {
    let request = http::request(http, Method::GET, &VERIFY, VERIFY_PATH).bearer_auth(access_token.expose());
    let answer: VerifyAnswer = http::send(request, &VERIFY).await?;
    if answer.error_message.is_some() {
        return Err(CallError::Revoked {
            call: VERIFY.name,
            status: 200,
            code: http::shown(answer.error_code.as_deref()),
        });
    }
    if answer.account_id.is_some_and(|verified| verified != account_id) {
        return Err(CallError::Malformed {
            call: VERIFY.name,
            reason: "the token belongs to another account".to_owned(),
        });
    }
    Ok(())
}

async fn grant(http: &reqwest::Client, call: &Call, form: &[(&str, &str); 2]) -> Result<Grant, CallError> {
    let request = http::request(http, Method::POST, call, TOKEN_PATH)
        .basic_auth(CLIENT_ID, Some(CLIENT_SECRET))
        .form(&[form[0], form[1], ("token_type", "eg1")]);
    let answer: TokenAnswer = http::send(request, call).await?;
    let malformed = |reason: &str| CallError::Malformed {
        call: call.name,
        reason: reason.to_owned(),
    };
    if SecretKey::epic_session(&answer.account_id).is_err() {
        return Err(malformed("account_id is not an id decanter can file a session under"));
    }
    if !is_token(&answer.access_token) || !is_token(&answer.refresh_token) {
        return Err(malformed("a token is empty, too long or not visible ASCII"));
    }
    let expires_in = expires_in(answer.expires_in.as_ref(), answer.expires_at.as_deref())
        .ok_or_else(|| malformed("neither expires_in nor expires_at is a lifetime up to a month"))?;
    Ok(Grant {
        display_name: answer
            .display_name
            .as_deref()
            .and_then(display_name)
            .unwrap_or_else(|| answer.account_id.clone()),
        account_id: answer.account_id,
        access_token: answer.access_token,
        refresh_token: answer.refresh_token,
        expires_in,
    })
}

/// `expires_in` counts from when the answer arrived, so a skewed clock does not turn it into a
/// token that looks expired or eternal; `expires_at` is the fallback (spec, 13.5).
fn expires_in(relative: Option<&serde_json::Number>, absolute: Option<&str>) -> Option<u64> {
    let from_now = || {
        let at = chrono::DateTime::parse_from_rfc3339(absolute?).ok()?;
        u64::try_from(at.timestamp().checked_sub(chrono::Utc::now().timestamp())?).ok()
    };
    relative
        .and_then(serde_json::Number::as_u64)
        .or_else(from_now)
        .filter(|seconds| (1..=MAX_EXPIRES_IN).contains(seconds))
}

/// Tokens go back as form values and bearer headers, so only visible ASCII passes.
pub(crate) fn is_token(token: &Secret) -> bool {
    let value = token.expose();
    (1..=TOKEN_MAX_LEN).contains(&value.len()) && value.bytes().all(|byte| byte.is_ascii_graphic())
}

/// The name is shown on screen and written in the database: control characters go, and a name
/// left empty or too long is not used.
fn display_name(given: &str) -> Option<String> {
    let printable: String = given.chars().filter(|c| !c.is_control()).collect();
    let name = printable.trim();
    (!name.is_empty() && name.chars().count() <= DISPLAY_NAME_MAX_CHARS).then(|| name.to_owned())
}

pub(crate) fn secret<'de, D>(deserializer: D) -> Result<Secret, D::Error>
where
    D: Deserializer<'de>,
{
    String::deserialize(deserializer).map(Secret::from)
}
