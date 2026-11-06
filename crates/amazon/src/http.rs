// SPDX-License-Identifier: GPL-3.0-or-later
//! The HTTP client of every call to Amazon's auth service, shaped as `decanter-gog`'s: one flat
//! timeout, HTTPS only, redirects held to the host asked, statuses read apart and bodies capped.

use core::time::Duration;

use reqwest::{RequestBuilder, Response, StatusCode, redirect};
use serde::de::DeserializeOwned;

use crate::error::CallError;
use crate::identity::{API_HOST, USER_AGENT};

/// One kind of request: its name in errors, the statuses that mean Amazon refused the
/// credentials, and the largest body it may answer with. Every answer here holds or concerns a
/// token, so no parse error quotes the body.
pub(crate) struct Call {
    pub name: &'static str,
    pub refused: &'static [StatusCode],
    pub max_body: usize,
}

pub(crate) fn url(path: &str) -> String {
    format!("https://{API_HOST}{path}")
}

/// reqwest's own default has no timeout, so an answer Amazon stalls on would hang the caller
/// forever; every call here is a small JSON round trip. Redirects may only stay on
/// [`API_HOST`] over HTTPS, since tokens ride along in the body and the bearer header.
pub(crate) fn client() -> Result<reqwest::Client, CallError> {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .https_only(true)
        .user_agent(USER_AGENT)
        .redirect(redirect::Policy::custom(|attempt| {
            let is_api = attempt.url().scheme() == "https" && attempt.url().host_str() == Some(API_HOST);
            if attempt.previous().len() >= 5 {
                attempt.error("too many redirects")
            } else if is_api {
                attempt.follow()
            } else {
                attempt.error("redirected off Amazon's auth service")
            }
        }))
        .build()
        .map_err(CallError::Setup)
}

/// Sends `request` and reads a 2xx body of at most `call.max_body` bytes as `T`.
pub(crate) async fn send<T>(request: RequestBuilder, call: &Call) -> Result<T, CallError>
where
    T: DeserializeOwned,
{
    let response = receive(request, call).await?;
    let body = read(response, call).await?;
    serde_json::from_slice(&body).map_err(|err| CallError::Malformed {
        call: call.name,
        reason: format!("not the expected JSON at line {}, column {}", err.line(), err.column()),
    })
}

/// Sends `request` and reads nothing but its status.
pub(crate) async fn send_empty(request: RequestBuilder, call: &Call) -> Result<(), CallError> {
    receive(request, call).await.map(drop)
}

async fn receive(request: RequestBuilder, call: &Call) -> Result<Response, CallError> {
    let response = request.send().await.map_err(|err| CallError::network(call.name, err))?;
    let status = response.status();
    if call.refused.contains(&status) {
        return Err(CallError::Rejected {
            call: call.name,
            status: status.as_u16(),
        });
    }
    if !status.is_success() {
        return Err(CallError::Unavailable {
            call: call.name,
            status: status.as_u16(),
        });
    }
    Ok(response)
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
        .map_err(|err| CallError::network(call.name, err))?
    {
        if body.len().saturating_add(chunk.len()) > call.max_body {
            return Err(too_large());
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}
