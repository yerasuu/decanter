// SPDX-License-Identifier: GPL-3.0-or-later
//! What talking to mod.io or staging its archives can end in. No variant carries a URL or the
//! API key: the key travels in the query string, and reqwest's errors are stripped of theirs.

use std::io;
use std::path::PathBuf;

/// mod.io's `error_ref` for a request that needs its terms of use accepted first (Terms and user
/// consent page, analysis section 3).
pub(crate) const TERMS_REF: u32 = 11074;

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// No key saved in Ajustes, or one that is not the shape of a key.
    #[error("no usable mod.io API key is saved")]
    NoKey,
    /// The request never got an answer: offline, DNS, TLS or a timeout.
    #[error("mod.io could not be reached for {call}")]
    Network {
        call: &'static str,
        #[source]
        source: reqwest::Error,
    },
    /// A redirect left mod.io's hosts or HTTPS, or went on too long.
    #[error("mod.io's answer to {call} redirected somewhere decanter does not follow")]
    Redirected { call: &'static str },
    /// HTTP 401: the key is not one mod.io knows, or it was revoked.
    #[error("mod.io refused the API key for {call} (HTTP 401)")]
    KeyRejected { call: &'static str },
    /// HTTP 403 with error 11074: the user has to accept mod.io's terms first.
    #[error("mod.io asks for its terms of use to be accepted before {call}")]
    TermsRequired { call: &'static str },
    #[error("mod.io refused {call} (HTTP 403, error {error_ref})")]
    Forbidden { call: &'static str, error_ref: u32 },
    /// HTTP 429. Every request is refused here until `seconds` have passed, without asking.
    #[error("mod.io asks decanter to wait {seconds} s before {call}")]
    RateLimited { call: &'static str, seconds: u64 },
    #[error("mod.io could not answer {call} (HTTP {status})")]
    Unavailable { call: &'static str, status: u16 },
    #[error("mod.io has no {call} for this (HTTP 404)")]
    NotFound { call: &'static str },
    /// A 2xx whose body is not the shape decanter reads.
    #[error("mod.io's answer to {call} is not what decanter reads: {reason}")]
    Malformed { call: &'static str, reason: String },
    #[error("mod.io's answer to {call} is larger than {limit} bytes")]
    TooLarge { call: &'static str, limit: u64 },
    /// The game's own record does not have `api_access_options` bit 1.
    #[error("this game does not allow third-party tools on mod.io")]
    NoThirdPartyAccess,
    /// The game's record has bit 4 or 8: downloads need a logged-in user, which decanter does not do.
    #[error("this game's mod.io downloads need a logged-in mod.io user")]
    NeedsLogin,
    #[error("the mod has no file to download")]
    NoFile,
    /// The file's `virus_positive` is 1: mod.io's scan found it malicious.
    #[error("mod.io's virus scan flagged this file as malicious")]
    Malicious,
    #[error("mod.io's download link for this file has already expired")]
    LinkExpired,
    #[error("the download is {got} bytes and mod.io lists {expected}")]
    SizeMismatch { expected: u64, got: u64 },
    #[error("the download's MD5 is not the one mod.io lists")]
    HashMismatch,
    #[error("the download needs {needed} bytes and the library's disk has {free} free")]
    NoSpace { needed: u64, free: u64 },
    #[error("{}: {source}", path.display())]
    Io { path: PathBuf, source: io::Error },
    #[error("{}: {reason}", path.display())]
    Settings { path: PathBuf, reason: String },
    #[error("{0:?} is not a mod.io id")]
    InvalidId(String),
    #[error("building the HTTP client failed")]
    Setup(#[source] reqwest::Error),
    #[error("cancelled")]
    Cancelled,
    #[error(transparent)]
    Mods(#[from] decanter_mods::Error),
}

impl Error {
    /// Whether asking again later may succeed: an outage, a timeout, a wait mod.io asked for or an
    /// answer of an unexpected shape, against a refusal that would come back the same.
    #[must_use]
    #[inline]
    pub const fn is_retryable(&self) -> bool {
        match *self {
            Self::Network { .. }
            | Self::Malformed { .. }
            | Self::SizeMismatch { .. }
            | Self::HashMismatch
            | Self::LinkExpired
            | Self::RateLimited { .. } => true,
            Self::Unavailable { status, .. } => status >= 500 || status == 408,
            Self::NoKey
            | Self::Redirected { .. }
            | Self::KeyRejected { .. }
            | Self::TermsRequired { .. }
            | Self::Forbidden { .. }
            | Self::NotFound { .. }
            | Self::TooLarge { .. }
            | Self::NoThirdPartyAccess
            | Self::NeedsLogin
            | Self::NoFile
            | Self::Malicious
            | Self::NoSpace { .. }
            | Self::Io { .. }
            | Self::Settings { .. }
            | Self::InvalidId(_)
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
