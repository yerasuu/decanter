// SPDX-License-Identifier: GPL-3.0-or-later
//! What talking to Thunderstore or staging its archives can end in. No variant carries a URL: a
//! query string is the user's search, and reqwest's errors are stripped of theirs.

use std::io;
use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// The request never got an answer: offline, DNS, TLS or a timeout.
    #[error("Thunderstore could not be reached for {call}")]
    Network {
        call: &'static str,
        #[source]
        source: reqwest::Error,
    },
    /// A redirect left Thunderstore's hosts or HTTPS, or went on too long.
    #[error("Thunderstore's answer to {call} redirected somewhere decanter does not follow")]
    Redirected { call: &'static str },
    #[error("Thunderstore could not answer {call} (HTTP {status})")]
    Unavailable { call: &'static str, status: u16 },
    #[error("Thunderstore has no {call} for this (HTTP 404)")]
    NotFound { call: &'static str },
    /// A 2xx whose body is not the shape decanter reads. The API's schema is not published where
    /// decanter could read it, so this may be a change on Thunderstore's side as much as a fault.
    #[error("Thunderstore's answer to {call} is not what decanter reads: {reason}")]
    Malformed { call: &'static str, reason: String },
    #[error("Thunderstore's answer to {call} is larger than {limit} bytes")]
    TooLarge { call: &'static str, limit: u64 },
    #[error("the download is {got} bytes and Thunderstore lists {expected}")]
    SizeMismatch { expected: u64, got: u64 },
    #[error("the downloads need {needed} bytes and the library's disk has {free} free")]
    NoSpace { needed: u64, free: u64 },
    #[error("{}: {source}", path.display())]
    Io { path: PathBuf, source: io::Error },
    #[error("{}: {reason}", path.display())]
    Settings { path: PathBuf, reason: String },
    #[error("{0:?} is not a Thunderstore dependency (Team-Name-1.2.3)")]
    InvalidDependency(String),
    #[error("{0:?} is not a Thunderstore package name")]
    InvalidPackage(String),
    #[error("{0:?} is not a Thunderstore community")]
    InvalidCommunity(String),
    #[error("{0} is not in this game's Thunderstore community")]
    Unresolved(String),
    #[error("the package needs more than {0} packages in all")]
    TooManyDependencies(usize),
    #[error("the package's dependencies nest deeper than {0} levels")]
    TooDeep(usize),
    #[error("building the HTTP client failed")]
    Setup(#[source] reqwest::Error),
    #[error("cancelled")]
    Cancelled,
    #[error(transparent)]
    Mods(#[from] decanter_mods::Error),
}

impl Error {
    /// Whether asking again later may succeed: an outage, a timeout or an answer of an unexpected
    /// shape, against a refusal that would come back the same.
    #[must_use]
    #[inline]
    pub const fn is_retryable(&self) -> bool {
        match *self {
            Self::Network { .. } | Self::Malformed { .. } | Self::SizeMismatch { .. } => true,
            Self::Unavailable { status, .. } => status >= 500 || status == 429 || status == 408,
            Self::Redirected { .. }
            | Self::NotFound { .. }
            | Self::TooLarge { .. }
            | Self::NoSpace { .. }
            | Self::Io { .. }
            | Self::Settings { .. }
            | Self::InvalidDependency(_)
            | Self::InvalidPackage(_)
            | Self::InvalidCommunity(_)
            | Self::Unresolved(_)
            | Self::TooManyDependencies(_)
            | Self::TooDeep(_)
            | Self::Setup(_)
            | Self::Cancelled
            | Self::Mods(_) => false,
        }
    }

    pub(crate) fn network(call: &'static str, source: reqwest::Error) -> Self {
        if source.is_redirect() {
            return Self::Redirected { call };
        }
        Self::Network {
            call,
            source: source.without_url(),
        }
    }

    pub(crate) fn io(path: &std::path::Path) -> impl FnOnce(io::Error) -> Self + '_ {
        move |source| Self::Io {
            path: path.to_path_buf(),
            source,
        }
    }

    pub(crate) fn malformed(call: &'static str, reason: impl Into<String>) -> Self {
        Self::Malformed {
            call,
            reason: reason.into(),
        }
    }
}
