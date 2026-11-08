// SPDX-License-Identifier: GPL-3.0-or-later
//! The HTTP client of every call to Epic's services: HTTPS only, a connect and a total time limit,
//! redirects held to the host asked, bodies capped and statuses read into [`CallError`]'s classes.

use core::time::Duration;

use reqwest::header::USER_AGENT;
use reqwest::{Method, RequestBuilder, Response, StatusCode, redirect};
use serde::Deserialize;
use serde::de::DeserializeOwned;

use crate::error::CallError;
use crate::hosts::Host;

/// The `errorCode` with which Epic says the account must act on its website first (spec, 3.2).
const ACTION_REQUIRED: &str = "errors.com.epicgames.oauth.corrective_action_required";
/// Epic's codes are dotted identifiers; anything longer or stranger is not shown.
const CODE_MAX_LEN: usize = 128;
const MAX_REDIRECTS: usize = 5;

/// One kind of request: its name in errors, the host it goes to, how its refusals read and the
/// largest body it may answer with. Every answer here holds or concerns a token, so no parse
/// error quotes the body.
pub(crate) struct Call {
    pub name: &'static str,
    pub host: Host,
    pub kind: Kind,
    pub max_body: usize,
}

/// How a 4xx without a clear meaning reads.
pub(crate) enum Kind {
    /// The token endpoint, where a 4xx is a verdict on the grant (spec, 3.2).
    Grant,
    /// A call that bears an access token, where only 401 and 403, or one of Epic's error bodies,
    /// are a verdict on it (spec, 2.3 and 13.4).
    Bearer,
}

#[derive(Deserialize)]
struct ErrorAnswer {
    #[serde(rename = "errorCode", default)]
    code: Option<String>,
    #[serde(rename = "errorMessage", default)]
    message: Option<String>,
    #[serde(rename = "correctiveAction", default)]
    action: Option<String>,
}

/// One client for every host. reqwest's default has no time limit, and Epic has been seen to
/// stall a call for good rather than fail it (spec, section 12), so a stall must end as
/// [`CallError::TimedOut`] instead of hanging whoever waits on it. Redirects may only stay on the
/// host asked over HTTPS, since credentials ride in the headers and the form body.
pub(crate) fn client() -> Result<reqwest::Client, CallError> {
    reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(30))
        .https_only(true)
        .redirect(redirect::Policy::custom(|attempt| {
            let asked = attempt.previous().first().and_then(|url| url.host_str());
            let stays = attempt.url().scheme() == "https" && attempt.url().host_str() == asked;
            if attempt.previous().len() > MAX_REDIRECTS {
                attempt.error("too many redirects")
            } else if stays {
                attempt.follow()
            } else {
                attempt.error("redirected off the Epic host asked")
            }
        }))
        .build()
        .map_err(CallError::Setup)
}

/// A request for `path` on the call's host, under the user agent the host table gives it.
pub(crate) fn request(http: &reqwest::Client, method: Method, call: &Call, path: &str) -> RequestBuilder {
    http.request(method, call.host.url(path))
        .header(USER_AGENT, call.host.user_agent())
}

/// Sends `request` and reads a 2xx body of at most `call.max_body` bytes as `T`.
pub(crate) async fn send<T>(request: RequestBuilder, call: &Call) -> Result<T, CallError>
where
    T: DeserializeOwned,
{
    let response = request
        .send()
        .await
        .map_err(|err| CallError::transport(call.name, call.host, err))?;
    let status = response.status();
    if status.is_success() {
        let body = read(response, call).await?;
        return serde_json::from_slice(&body).map_err(|err| CallError::Malformed {
            call: call.name,
            reason: format!("not the expected JSON at line {}, column {}", err.line(), err.column()),
        });
    }
    // An error body that is too large or cut short is read as no body: the status still decides.
    let body = read(response, call).await.unwrap_or_default();
    Err(refusal(status, &body, call))
}

/// What a non-2xx status means. Epic's error answers are JSON with `errorCode` and
/// `errorMessage` (spec, 3.2 and 3.3); a 404 alone is a route that moved, not a dead session, since
/// Epic answers unknown routes that way (spec, sections 12 and 13.2).
fn refusal(status: StatusCode, body: &[u8], call: &Call) -> CallError {
    let unavailable = CallError::Unavailable {
        call: call.name,
        status: status.as_u16(),
    };
    if status.is_server_error() || status == StatusCode::TOO_MANY_REQUESTS {
        return unavailable;
    }
    let answer = serde_json::from_slice::<ErrorAnswer>(body)
        .ok()
        .filter(|parsed| parsed.code.is_some() || parsed.message.is_some());
    let code = answer.as_ref().and_then(|parsed| shown(parsed.code.as_deref()));
    if code.as_deref() == Some(ACTION_REQUIRED) {
        return CallError::ActionRequired {
            call: call.name,
            action: answer.and_then(|parsed| shown(parsed.action.as_deref())),
        };
    }
    let revoked = CallError::Revoked {
        call: call.name,
        status: status.as_u16(),
        code,
    };
    let is_grant = matches!(call.kind, Kind::Grant);
    if matches!(status, StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN) {
        revoked
    } else if matches!(status, StatusCode::NOT_FOUND | StatusCode::GONE) && (!is_grant || answer.is_none()) {
        CallError::Moved {
            call: call.name,
            host: call.host,
            status: status.as_u16(),
        }
    } else if status.is_client_error() && (is_grant || answer.is_some()) {
        revoked
    } else {
        unavailable
    }
}

/// An identifier from Epic's error answer, kept only when it looks like one.
pub(crate) fn shown(value: Option<&str>) -> Option<String> {
    value
        .filter(|text| {
            (1..=CODE_MAX_LEN).contains(&text.len())
                && text
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b':'))
        })
        .map(str::to_owned)
}

/// The body of `response`, refused once it grows past `call.max_body` bytes.
async fn read(mut response: Response, call: &Call) -> Result<Vec<u8>, CallError> {
    let too_large = || CallError::Malformed {
        call: call.name,
        reason: format!("body larger than {} bytes", call.max_body),
    };
    let declared = response
        .content_length()
        .and_then(|length| usize::try_from(length).ok());
    if declared.is_some_and(|length| length > call.max_body) {
        return Err(too_large());
    }
    let mut body = Vec::with_capacity(declared.unwrap_or_default());
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|err| CallError::transport(call.name, call.host, err))?
    {
        if body.len().saturating_add(chunk.len()) > call.max_body {
            return Err(too_large());
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}
