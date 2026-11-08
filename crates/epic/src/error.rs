// SPDX-License-Identifier: GPL-3.0-or-later
//! What a call to Epic can end in. Three outcomes are kept apart because they ask for different
//! things of the user: [`CallError::Revoked`] alone means logging in again;
//! [`CallError::Moved`] and [`CallError::Unreachable`] mean the route or the network, which a new
//! login does not fix; and [`CallError::TimedOut`] says nothing about either, since Epic has been
//! seen to stall on a valid token (spec, sections 12 and 13.1).
//!
//! No variant carries a URL, a token or a response body: reqwest's errors lose their URL here, and
//! Epic's error answers are reduced to their `errorCode`.

use crate::hosts::Host;

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum CallError {
    /// Epic turned the credentials down: only a new browser login brings the session back.
    /// `code` is Epic's `errorCode`, when its answer named one decanter can show.
    #[error("Epic refused the session at {call} (HTTP {status}, {})", .code.as_deref().unwrap_or("no error code"))]
    Revoked {
        call: &'static str,
        status: u16,
        code: Option<String>,
    },
    /// The account must do something on Epic's website first (accept new terms, set up a second
    /// factor); until then Epic refuses to sign it in.
    #[error(
        "Epic asks for an action on its website before {call} ({}): finish it at epicgames.com, then retry",
        .action.as_deref().unwrap_or("unnamed")
    )]
    ActionRequired { call: &'static str, action: Option<String> },
    /// No answer within the call's time limit. Not evidence about the session.
    #[error("Epic's {} did not answer {call} in time", .host.name())]
    TimedOut { call: &'static str, host: Host },
    /// The request never got an answer: offline, DNS, TLS, a redirect off the host asked.
    #[error("Epic's {} could not be reached for {call}", .host.name())]
    Unreachable {
        call: &'static str,
        host: Host,
        #[source]
        source: reqwest::Error,
    },
    /// The host answered that the route is not there (HTTP 404 or 410 without one of Epic's error
    /// bodies): the endpoint moved, which only a decanter release with a new host table fixes.
    #[error("Epic's {} no longer serves {call} (HTTP {status})", .host.name())]
    Moved {
        call: &'static str,
        host: Host,
        status: u16,
    },
    /// A status that says nothing about the credentials (a 5xx, a 429).
    #[error("Epic could not answer {call} (HTTP {status})")]
    Unavailable { call: &'static str, status: u16 },
    /// A successful answer whose body is too large or not the expected shape.
    #[error("Epic's answer to {call} is malformed: {reason}")]
    Malformed { call: &'static str, reason: String },
    /// The HTTP client could not be built.
    #[error("building the Epic HTTP client failed")]
    Setup(#[source] reqwest::Error),
}

impl CallError {
    /// reqwest puts the URL in its errors, and a token or a code may ride in one; a timeout is
    /// told apart here since it is the one failure Epic is known to produce on a valid session.
    pub(crate) fn transport(call: &'static str, host: Host, source: reqwest::Error) -> Self {
        if source.is_timeout() {
            Self::TimedOut { call, host }
        } else {
            Self::Unreachable {
                call,
                host,
                source: source.without_url(),
            }
        }
    }
}
