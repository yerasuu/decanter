// SPDX-License-Identifier: GPL-3.0-or-later
//! What a call to Amazon can end in, split as `decanter-gog` splits GOG's: only
//! [`CallError::Rejected`] means the user has to log in again.

#[derive(Debug, thiserror::Error)]
pub(crate) enum CallError {
    /// Amazon turned the credentials down: only a new browser login brings the session back.
    #[error("Amazon rejected the session at {call} (HTTP {status})")]
    Rejected { call: &'static str, status: u16 },
    /// The request never got an answer: offline, DNS, TLS, a timeout or a redirect off Amazon.
    #[error("Amazon could not be reached for {call}")]
    Network {
        call: &'static str,
        #[source]
        source: reqwest::Error,
    },
    /// A status that says nothing about the credentials (a 5xx, a 404, a 429).
    #[error("Amazon could not answer {call} (HTTP {status})")]
    Unavailable { call: &'static str, status: u16 },
    /// A successful answer whose body is too large or not the expected shape.
    #[error("Amazon's answer to {call} is malformed: {reason}")]
    Malformed { call: &'static str, reason: String },
    /// The HTTP client could not be built.
    #[error("building the Amazon HTTP client failed")]
    Setup(#[source] reqwest::Error),
    /// The system's random source failed, so no PKCE verifier can be made.
    #[error("the system random source failed: {0}")]
    Random(String),
}

impl CallError {
    /// reqwest puts the URL in its errors; none of these carries a credential today, but the
    /// sign-in flow's do, and an error must never become the place one leaks from.
    pub(crate) fn network(call: &'static str, source: reqwest::Error) -> Self {
        Self::Network {
            call,
            source: source.without_url(),
        }
    }
}
