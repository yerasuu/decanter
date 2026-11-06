// SPDX-License-Identifier: GPL-3.0-or-later
//! The three calls of Amazon's auth service: registering decanter as a launcher device, renewing
//! its access token and deregistering it.
//!
//! Endpoints, field names and values are nile's as docs/amazon-nile-integration-analysis.md
//! sections 2.1 to 2.3 read them; nothing Amazon publishes describes them, so every shape is
//! UNVERIFIED until the owner's spike (kb #1519). Where the analysis leaves a detail open the
//! choice is marked INFER: the refresh and deregistration bodies are sent as JSON, and the refresh
//! answer is read as a top-level `access_token` and `expires_in`.

use decanter_core::secrets::{Secret, SecretKey};
use reqwest::StatusCode;
use serde::{Deserialize, Deserializer, Serialize};

use crate::error::CallError;
use crate::http::{self, Call};
use crate::identity::{APP_NAME, APP_VERSION, DEVICE_MODEL, DEVICE_TYPE, OS_VERSION};

/// A refused or spent grant is an `invalid_grant` error, which
/// [RFC 6749, section 5.2](https://www.rfc-editor.org/rfc/rfc6749#section-5.2) answers with 400;
/// Amazon's own status for it is UNVERIFIED.
const GRANT_REFUSED: &[StatusCode] = &[StatusCode::BAD_REQUEST, StatusCode::UNAUTHORIZED, StatusCode::FORBIDDEN];
const TOKEN_REFUSED: &[StatusCode] = &[StatusCode::UNAUTHORIZED, StatusCode::FORBIDDEN];

const REGISTER: Call = Call {
    name: "device registration",
    refused: GRANT_REFUSED,
    max_body: 64 << 10,
};
const REFRESH: Call = Call {
    name: "token refresh",
    refused: GRANT_REFUSED,
    max_body: 64 << 10,
};
const DEREGISTER: Call = Call {
    name: "device deregistration",
    refused: TOKEN_REFUSED,
    max_body: 0,
};

/// Tokens go into a JSON body and a bearer header; nothing Amazon issues needs more.
const TOKEN_MAX_LEN: usize = 8 << 10;
/// A month: an access token is expected to last an hour (UNVERIFIED), and one claiming longer
/// is not one decanter should trust to stay valid unrenewed.
const MAX_EXPIRES_IN: u64 = 30 * 24 * 3600;
const DISPLAY_NAME_MAX_CHARS: usize = 256;

/// Who the registration answered for, and the device's first tokens.
pub(crate) struct Registration {
    pub account_id: String,
    pub display_name: String,
    pub access_token: Secret,
    pub refresh_token: Secret,
    pub expires_in: u64,
}

/// A renewed access token. Amazon does not rotate the refresh token here (analysis, section 2.2:
/// nile reads none back), so the stored one stays.
pub(crate) struct Renewal {
    pub access_token: Secret,
    pub expires_in: u64,
}

/// What the device is registered as; the serial and client id are made once per login.
pub(crate) struct Device<'req> {
    pub serial: &'req str,
    pub client_id: &'req str,
}

#[derive(Serialize)]
struct RegisterRequest<'req> {
    auth_data: AuthData<'req>,
    registration_data: RegistrationData<'req>,
    requested_extensions: [&'static str; 2],
    /// nile asks for a `mac_dms` device key as well and never uses it (analysis, U8); asking for
    /// it keeps the request the launcher's, and decanter drops it unread.
    requested_token_type: [&'static str; 2],
}

#[derive(Serialize)]
struct AuthData<'req> {
    authorization_code: &'req str,
    client_domain: &'static str,
    client_id: &'req str,
    code_algorithm: &'static str,
    code_verifier: &'req str,
    use_global_authentication: bool,
}

#[derive(Serialize)]
struct RegistrationData<'req> {
    app_name: &'static str,
    app_version: &'static str,
    device_model: &'static str,
    device_name: Option<&'static str>,
    device_serial: &'req str,
    device_type: &'static str,
    domain: &'static str,
    os_version: &'static str,
}

#[derive(Serialize)]
struct RefreshRequest<'req> {
    source_token: &'req str,
    source_token_type: &'static str,
    requested_token_type: &'static str,
    app_name: &'static str,
    app_version: &'static str,
}

#[derive(Deserialize)]
struct RegisterAnswer {
    response: RegisterResponse,
}

#[derive(Deserialize)]
struct RegisterResponse {
    success: RegisterSuccess,
}

#[derive(Deserialize)]
struct RegisterSuccess {
    tokens: Tokens,
    extensions: Extensions,
}

#[derive(Deserialize)]
struct Tokens {
    bearer: Bearer,
}

#[derive(Deserialize)]
struct Bearer {
    #[serde(deserialize_with = "secret")]
    access_token: Secret,
    #[serde(deserialize_with = "secret")]
    refresh_token: Secret,
    expires_in: Seconds,
}

#[derive(Deserialize)]
struct Extensions {
    customer_info: CustomerInfo,
    #[serde(default)]
    device_info: Option<DeviceInfo>,
}

#[derive(Deserialize)]
struct CustomerInfo {
    user_id: String,
    #[serde(default)]
    given_name: Option<String>,
}

#[derive(Deserialize)]
struct DeviceInfo {
    #[serde(default)]
    device_serial_number: Option<String>,
}

#[derive(Deserialize)]
struct RefreshAnswer {
    #[serde(deserialize_with = "secret")]
    access_token: Secret,
    expires_in: Seconds,
}

/// nile turns `expires_in` into an integer before using it, so whether Amazon sends a number or
/// a decimal string is UNVERIFIED: both are read.
#[derive(Deserialize)]
#[serde(untagged)]
enum Seconds {
    Number(u64),
    Text(String),
}

/// Trades the one-shot authorization code of the sign-in redirect, with the PKCE verifier it was
/// asked for under, for a registered device and its tokens.
pub(crate) async fn register(
    http: &reqwest::Client,
    code: &Secret,
    verifier: &Secret,
    device: &Device<'_>,
) -> Result<Registration, CallError> {
    let body = RegisterRequest {
        auth_data: AuthData {
            authorization_code: code.expose(),
            client_domain: "DeviceLegacy",
            client_id: device.client_id,
            code_algorithm: "SHA-256",
            code_verifier: verifier.expose(),
            use_global_authentication: false,
        },
        registration_data: RegistrationData {
            app_name: APP_NAME,
            app_version: APP_VERSION,
            device_model: DEVICE_MODEL,
            device_name: None,
            device_serial: device.serial,
            device_type: DEVICE_TYPE,
            domain: "Device",
            os_version: OS_VERSION,
        },
        requested_extensions: ["customer_info", "device_info"],
        requested_token_type: ["bearer", "mac_dms"],
    };
    let answer: RegisterAnswer = http::send(http.post(http::url("/auth/register")).json(&body), &REGISTER).await?;
    let RegisterSuccess { tokens, extensions } = answer.response.success;
    let malformed = |reason: &str| CallError::Malformed {
        call: REGISTER.name,
        reason: reason.to_owned(),
    };
    let account_id = extensions.customer_info.user_id;
    if SecretKey::amazon_session(&account_id).is_err() {
        return Err(malformed("user_id is not an id decanter can file a session under"));
    }
    let serial_matches = extensions
        .device_info
        .and_then(|info| info.device_serial_number)
        .is_none_or(|returned| returned.eq_ignore_ascii_case(device.serial));
    if !serial_matches {
        // The serial sent is the one the client id was derived from, so it is the one kept; what
        // Amazon's answer means when they differ is UNVERIFIED.
        tracing::warn!(
            call = REGISTER.name,
            "amazon registered the device under another serial"
        );
    }
    Ok(Registration {
        display_name: extensions
            .customer_info
            .given_name
            .as_deref()
            .and_then(display_name)
            .unwrap_or_else(|| account_id.clone()),
        account_id,
        access_token: token(tokens.bearer.access_token, &REGISTER)?,
        refresh_token: token(tokens.bearer.refresh_token, &REGISTER)?,
        expires_in: seconds(tokens.bearer.expires_in, &REGISTER)?,
    })
}

/// Renews the access token with the device's refresh token.
pub(crate) async fn refresh(http: &reqwest::Client, refresh_token: &Secret) -> Result<Renewal, CallError> {
    let body = RefreshRequest {
        source_token: refresh_token.expose(),
        source_token_type: "refresh_token",
        requested_token_type: "access_token",
        app_name: APP_NAME,
        app_version: APP_VERSION,
    };
    let answer: RefreshAnswer = http::send(http.post(http::url("/auth/token")).json(&body), &REFRESH).await?;
    Ok(Renewal {
        access_token: token(answer.access_token, &REFRESH)?,
        expires_in: seconds(answer.expires_in, &REFRESH)?,
    })
}

/// Removes the device from the user's Amazon account, so that logging out leaves no registered
/// device behind (analysis, section 2.3).
pub(crate) async fn deregister(http: &reqwest::Client, access_token: &Secret) -> Result<(), CallError> {
    let request = http
        .post(http::url("/auth/deregister"))
        .bearer_auth(access_token.expose())
        .json(&serde_json::Map::new());
    http::send_empty(request, &DEREGISTER).await
}

/// Tokens are sent back as JSON strings and bearer headers, so only visible ASCII passes.
pub(crate) fn is_token(token: &Secret) -> bool {
    let value = token.expose();
    (1..=TOKEN_MAX_LEN).contains(&value.len()) && value.bytes().all(|byte| byte.is_ascii_graphic())
}

fn token(token: Secret, call: &Call) -> Result<Secret, CallError> {
    if is_token(&token) {
        Ok(token)
    } else {
        Err(CallError::Malformed {
            call: call.name,
            reason: "a token is empty, too long or not visible ASCII".to_owned(),
        })
    }
}

fn seconds(seconds: Seconds, call: &Call) -> Result<u64, CallError> {
    let parsed = match seconds {
        Seconds::Number(number) => Some(number),
        Seconds::Text(text) => text.parse().ok(),
    };
    parsed
        .filter(|number| (1..=MAX_EXPIRES_IN).contains(number))
        .ok_or_else(|| CallError::Malformed {
            call: call.name,
            reason: format!("expires_in is not a number of seconds up to {MAX_EXPIRES_IN}"),
        })
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
