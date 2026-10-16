//! Why a port's upstream could not be asked, told apart so the Ports view
//! says what the user can do about it instead of a raw git or HTTP error.

use crate::prebuilt::LookupError;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RemoteError {
    #[error("no connection to the remote: {0}")]
    Offline(String),
    #[error("the remote is rate limiting requests")]
    RateLimited,
    #[error("the remote repository is gone or moved: {0}")]
    Missing(String),
    #[error("{0}")]
    Failed(String),
}

impl RemoteError {
    /// Read off git's stderr, which [`crate::updater`] keeps in English
    /// (`LC_ALL=C`) so these matches hold whatever the user's locale.
    pub fn from_git(stderr: String) -> Self {
        const OFFLINE: [&str; 6] = [
            "Could not resolve host",
            "Failed to connect",
            "Couldn't connect to server",
            "Connection timed out",
            "Operation timed out",
            "Network is unreachable",
        ];
        const MISSING: [&str; 3] = ["Repository not found", "could not read Username", "returned error: 404"];
        if OFFLINE.iter().any(|needle| stderr.contains(needle)) {
            Self::Offline(stderr)
        } else if stderr.contains("returned error: 429") {
            Self::RateLimited
        } else if MISSING.iter().any(|needle| stderr.contains(needle)) {
            Self::Missing(stderr)
        } else {
            Self::Failed(stderr)
        }
    }
}

impl From<LookupError> for RemoteError {
    fn from(err: LookupError) -> Self {
        match err {
            LookupError::Request(request) if request.is_connect() || request.is_timeout() => {
                Self::Offline(request.to_string())
            }
            LookupError::Request(request)
                if matches!(
                    request.status(),
                    Some(reqwest::StatusCode::TOO_MANY_REQUESTS | reqwest::StatusCode::FORBIDDEN)
                ) =>
            {
                Self::RateLimited
            }
            other @ (LookupError::Request(_)
            | LookupError::Read(_)
            | LookupError::Malformed(_)
            | LookupError::MalformedRelease(_)) => Self::Failed(other.to_string()),
        }
    }
}
