// SPDX-License-Identifier: GPL-3.0-or-later
//! What talking to Nexus Mods or staging its archives can end in. No variant carries a URL or the
//! key: a download link holds a signed query, and reqwest's errors are stripped of theirs.

use std::io;
use std::path::PathBuf;

use decanter_core::secrets::StoreError;

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    #[error("no Nexus Mods API key is saved")]
    NoKey,
    /// 401: Nexus does not take the key, which only the user can replace.
    #[error("Nexus Mods does not accept the saved API key")]
    KeyRefused,
    /// 403 on a download link: Nexus gives one through the API to Premium accounts only.
    #[error("Nexus Mods gives download links through the API to Premium accounts only")]
    PremiumOnly,
    /// 429, or the rate-limit headers counted both the daily and the hourly allowance out.
    #[error("the Nexus Mods request limit of this account is used up until {resets}")]
    RateLimited { resets: String },
    #[error("the keyring: {0}")]
    Keyring(#[source] StoreError),
    /// The request never got an answer: offline, DNS, TLS or a timeout.
    #[error("Nexus Mods could not be reached for {call}")]
    Network {
        call: &'static str,
        #[source]
        source: reqwest::Error,
    },
    /// A redirect left the hosts decanter follows for that request, or went on too long.
    #[error("Nexus Mods' answer to {call} redirected somewhere decanter does not follow")]
    Redirected { call: &'static str },
    #[error("Nexus Mods could not answer {call} (HTTP {status})")]
    Unavailable { call: &'static str, status: u16 },
    #[error("Nexus Mods has no {call} for this (HTTP 404)")]
    NotFound { call: &'static str },
    /// A 2xx whose body is not the shape decanter reads. The v1 reference was not readable for
    /// the analysis, so this may be a change on Nexus' side as much as a fault.
    #[error("Nexus Mods' answer to {call} is not what decanter reads: {reason}")]
    Malformed { call: &'static str, reason: String },
    #[error("Nexus Mods' answer to {call} is larger than {limit} bytes")]
    TooLarge { call: &'static str, limit: u64 },
    #[error("the download is {got} bytes and Nexus Mods lists {expected}")]
    SizeMismatch { expected: u64, got: u64 },
    /// The file's MD5 is not the one its listing gives: a damaged or replaced download. MD5 checks
    /// integrity only, not who made the file.
    #[error("the download's MD5 is not the one Nexus Mods lists for the file")]
    Md5Mismatch,
    #[error("the download needs {needed} bytes and the library's disk has {free} free")]
    NoSpace { needed: u64, free: u64 },
    #[error("{}: {source}", path.display())]
    Io { path: PathBuf, source: io::Error },
    #[error("{}: {reason}", path.display())]
    Settings { path: PathBuf, reason: String },
    #[error("{0:?} is not a Nexus Mods game domain")]
    InvalidDomain(String),
    #[error("{0:?} is neither a Nexus Mods mod number nor the address of a mod's page")]
    InvalidReference(String),
    #[error("building the HTTP client failed")]
    Setup(#[source] reqwest::Error),
    #[error("cancelled")]
    Cancelled,
    #[error(transparent)]
    Mods(#[from] decanter_mods::Error),
}

impl Error {
    /// Whether asking again later may succeed: an outage, a timeout, a used-up limit or an answer
    /// of an unexpected shape, against a refusal that would come back the same.
    #[must_use]
    #[inline]
    pub const fn is_retryable(&self) -> bool {
        match *self {
            Self::Network { .. }
            | Self::Malformed { .. }
            | Self::SizeMismatch { .. }
            | Self::Md5Mismatch
            | Self::RateLimited { .. } => true,
            Self::Unavailable { status, .. } => status >= 500 || status == 408,
            Self::NoKey
            | Self::KeyRefused
            | Self::PremiumOnly
            | Self::Keyring(_)
            | Self::Redirected { .. }
            | Self::NotFound { .. }
            | Self::TooLarge { .. }
            | Self::NoSpace { .. }
            | Self::Io { .. }
            | Self::Settings { .. }
            | Self::InvalidDomain(_)
            | Self::InvalidReference(_)
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
