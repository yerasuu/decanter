// SPDX-License-Identifier: GPL-3.0-or-later
//! What a call to GOG can end in, split the way `decanter-gog`'s session check reads its answers:
//! only [`Error::Rejected`] means the user has to log in again. The blocking client reports a 503
//! page and a revoked token alike as a JSON parse error, which is what this type exists to undo.

/// A failed call to GOG.
///
/// [`Error::Network`] and [`Error::Unavailable`] both mean GOG could not be asked, so nothing is
/// known about the session; [`Error::Malformed`] means GOG answered something this client cannot
/// read.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// GOG turned the credentials down (HTTP 401 or 403, a refused refresh, or `userData.json`
    /// saying nobody is logged in): only a new browser login brings the session back.
    #[error("GOG rejected the session at {call} ({reason})")]
    Rejected { call: &'static str, reason: String },
    /// The request never got an answer: offline, DNS, TLS, a timeout or a redirect off GOG.
    #[error("GOG could not be reached for {call}")]
    Network {
        call: &'static str,
        #[source]
        source: reqwest::Error,
    },
    /// GOG answered with a status that says nothing about the credentials (a 5xx, a 404, a 429).
    #[error("GOG could not answer {call} (HTTP {status})")]
    Unavailable { call: &'static str, status: u16 },
    /// A successful answer whose body is too large or not the expected shape.
    #[error("GOG's answer to {call} is malformed: {reason}")]
    Malformed { call: &'static str, reason: String },
    /// Input refused before anything was sent.
    #[error("invalid {what}")]
    InvalidInput { what: &'static str },
    /// The HTTP client itself could not be built.
    #[error("building the GOG HTTP client failed")]
    Setup(#[source] reqwest::Error),
}

impl Error {
    /// Request URLs carry the client secret, the refresh token or the login code, and reqwest
    /// puts the URL in its errors.
    pub(super) fn network(call: &'static str, source: reqwest::Error) -> Self {
        Self::Network {
            call,
            source: source.without_url(),
        }
    }
}
