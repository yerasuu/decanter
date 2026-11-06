// SPDX-License-Identifier: GPL-3.0-or-later
//! Where a game's files are: `GetGameDownload` on the distribution service, then `manifest.proto`
//! and `files/<sha256>` under the URL it answers (docs/amazon-nile-integration-analysis.md,
//! section 4.1). UNVERIFIED against Amazon: whether that URL is pre-signed, which host serves it and
//! whether it honours `Range` are what the owner's first install shows (U2).
//!
//! The URL may carry a signature, so it is never logged or put in an error: only its host is.

use core::fmt;
use core::time::Duration;
use std::sync::OnceLock;

use reqwest::StatusCode;
use reqwest::header::{HeaderName, HeaderValue};
use serde::{Deserialize, Serialize};
use url::{Host, Url};

use crate::error::CallError;
use crate::http::{self, Call};
use crate::identity::{DISTRIBUTION_PATH, DISTRIBUTION_USER_AGENT, DOWNLOAD_TARGET, GAMING_HOST};
use crate::library::is_id;
use crate::manifest::{MAX_RAW_LEN, Sha256Digest};
use crate::session::{self, Live, LiveSession};

const DOWNLOAD: Call = Call {
    name: "game download",
    refused: &[StatusCode::UNAUTHORIZED, StatusCode::FORBIDDEN],
    max_body: 64 << 10,
};

const URL_MAX_LEN: usize = 4096;

/// The domains a download URL may point at, and its redirects follow to. Which one Amazon uses is
/// not known yet (U2); a URL elsewhere is refused with its host logged, so the list is widened from
/// evidence rather than guessed open.
const DOWNLOAD_DOMAINS: [&str; 4] = ["amazon.com", "amazonaws.com", "cloudfront.net", "a2z.com"];

/// One version of a game and where its files are.
pub(super) struct GameDownload {
    pub(super) version_id: String,
    base: Url,
}

/// Without the URL, which may be signed.
impl fmt::Debug for GameDownload {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GameDownload")
            .field("version_id", &self.version_id)
            .field("host", &self.base.host_str())
            .finish_non_exhaustive()
    }
}

impl GameDownload {
    pub(super) fn host(&self) -> &str {
        self.base.host_str().unwrap_or_default()
    }

    pub(super) fn manifest_url(&self) -> Url {
        self.under(&["manifest.proto"])
    }

    /// Files are content-addressed: their name under `files/` is their lowercase hex SHA-256.
    pub(super) fn file_url(&self, sha256: &Sha256Digest) -> Url {
        self.under(&["files", &sha256.to_string()])
    }

    /// `segments` appended to the path, keeping the query a signed URL needs.
    fn under(&self, segments: &[&str]) -> Url {
        let mut url = self.base.clone();
        if let Ok(mut path) = url.path_segments_mut() {
            path.pop_if_empty().extend(segments);
        }
        url
    }
}

/// What asking for a download ended in.
pub(super) enum Asked {
    Ready(GameDownload),
    /// No session, or Amazon refused it: only a new login helps.
    SessionClosed,
    /// Amazon could not be asked or answered unusably; asking again later may work.
    Unreachable(String),
}

#[derive(Serialize)]
struct Request<'req> {
    #[serde(rename = "entitlementId")]
    entitlement_id: &'req str,
    #[serde(rename = "Operation")]
    operation: &'static str,
}

#[derive(Deserialize)]
struct Answer {
    #[serde(rename = "versionId")]
    version_id: String,
    #[serde(rename = "downloadUrl")]
    download_url: String,
}

/// Asks the distribution service where the entitlement's current version is.
///
/// # Errors
///
/// Only a local problem: the keyring, the HTTP client. Anything Amazon says is an [`Asked`].
pub(super) async fn game_download(account_id: &str, entitlement_id: &str) -> anyhow::Result<Asked> {
    let session = match session::live_session(account_id).await? {
        Live::Ready(session) => session,
        Live::LoggedOut | Live::Refused => return Ok(Asked::SessionClosed),
        Live::Unreachable(reason) => return Ok(Asked::Unreachable(reason)),
    };
    let http = http::client_for(GAMING_HOST, DISTRIBUTION_USER_AGENT)?;
    match ask(&http, &session, entitlement_id).await {
        Ok(download) => {
            tracing::info!(
                account_id,
                version_id = download.version_id,
                host = download.host(),
                has_query = download.base.query().is_some(),
                "amazon answered the game download"
            );
            Ok(Asked::Ready(download))
        }
        Err(CallError::Rejected { call, status }) => {
            tracing::info!(account_id, call, status, "amazon refused the session for a download");
            Ok(Asked::SessionClosed)
        }
        Err(err @ (CallError::Network { .. } | CallError::Unavailable { .. } | CallError::Malformed { .. })) => {
            let reason = session::reason(err);
            tracing::warn!(account_id, reason, "amazon did not answer the game download");
            Ok(Asked::Unreachable(reason))
        }
        Err(err @ (CallError::Setup(_) | CallError::Random(_))) => Err(anyhow::Error::new(err)),
    }
}

async fn ask(http: &reqwest::Client, session: &LiveSession, entitlement_id: &str) -> Result<GameDownload, CallError> {
    let mut token = HeaderValue::from_str(session.access_token.expose()).map_err(|err| CallError::Malformed {
        call: DOWNLOAD.name,
        reason: format!("the access token is not a valid header value: {err}"),
    })?;
    token.set_sensitive(true);
    let request = http
        .post(format!("https://{GAMING_HOST}{DISTRIBUTION_PATH}"))
        .header(HeaderName::from_static("x-amz-target"), DOWNLOAD_TARGET)
        .header(HeaderName::from_static("x-amzn-token"), token)
        .header("UserAgent", DISTRIBUTION_USER_AGENT)
        .header("Content-Encoding", "amz-1.0")
        .json(&Request {
            entitlement_id,
            operation: "GetGameDownload",
        });
    let answer: Answer = http::send(request, &DOWNLOAD).await?;
    let malformed = |reason: String| CallError::Malformed {
        call: DOWNLOAD.name,
        reason,
    };
    if !is_id(&answer.version_id) {
        return Err(malformed("the versionId is not a usable id".to_owned()));
    }
    let base = download_base(&answer.download_url).map_err(malformed)?;
    Ok(GameDownload {
        version_id: answer.version_id,
        base,
    })
}

/// The answer's `downloadUrl`, refused unless it is HTTPS to one of [`DOWNLOAD_DOMAINS`] by name,
/// with no credentials or fragment in it. The reason quotes the host at most.
fn download_base(raw: &str) -> Result<Url, String> {
    if raw.len() > URL_MAX_LEN {
        return Err(format!("the downloadUrl is longer than {URL_MAX_LEN} bytes"));
    }
    let url = Url::parse(raw).map_err(|err| format!("the downloadUrl does not parse: {err}"))?;
    if url.scheme() != "https" {
        return Err(format!("the downloadUrl is not HTTPS but {}", url.scheme()));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err("the downloadUrl carries credentials".to_owned());
    }
    if url.fragment().is_some() {
        return Err("the downloadUrl has a fragment".to_owned());
    }
    if url.port().is_some() {
        return Err("the downloadUrl names a port".to_owned());
    }
    match url.host() {
        Some(Host::Domain(domain)) if is_download_host(domain) => Ok(url),
        Some(Host::Domain(domain)) => {
            tracing::warn!(
                host = domain,
                "amazon's downloadUrl points at a host decanter does not download from"
            );
            Err(format!(
                "the downloadUrl points at {domain}, not an Amazon download host"
            ))
        }
        Some(Host::Ipv4(_) | Host::Ipv6(_)) | None => Err("the downloadUrl has no host name".to_owned()),
    }
}

fn is_download_host(host: &str) -> bool {
    let host = host.to_ascii_lowercase();
    DOWNLOAD_DOMAINS.iter().any(|domain| {
        host == *domain
            || host
                .strip_suffix(domain)
                .is_some_and(|label| label.ends_with('.') && label.len() > 1)
    })
}

/// The client of every request under a download URL. No overall timeout, since a file can take
/// hours; `read_timeout` bounds the time between reads instead, so a stalled CDN connection fails
/// rather than hangs. Redirects stay on HTTPS and on [`DOWNLOAD_DOMAINS`].
pub(super) fn client() -> anyhow::Result<&'static reqwest::Client> {
    static CLIENT: OnceLock<Option<reqwest::Client>> = OnceLock::new();
    CLIENT
        .get_or_init(|| {
            reqwest::Client::builder()
                .connect_timeout(Duration::from_secs(10))
                .read_timeout(Duration::from_secs(30))
                .https_only(true)
                .user_agent(DISTRIBUTION_USER_AGENT)
                .redirect(reqwest::redirect::Policy::custom(|attempt| {
                    let allowed =
                        attempt.url().scheme() == "https" && attempt.url().host_str().is_some_and(is_download_host);
                    if attempt.previous().len() >= 5 {
                        attempt.error("too many redirects")
                    } else if allowed {
                        attempt.follow()
                    } else {
                        tracing::warn!(
                            host = attempt.url().host_str(),
                            "amazon download redirected off its hosts"
                        );
                        attempt.error("redirected off Amazon's download hosts")
                    }
                }))
                .build()
                .inspect_err(|err| tracing::error!(error = %err, "building the amazon download client failed"))
                .ok()
        })
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("could not build the Amazon download client"))
}

/// Why `manifest.proto` could not be read; [`ManifestFetch::Unreachable`] is worth trying later.
pub(super) enum ManifestFetch {
    Unreachable(String),
    TooLarge,
}

/// The bytes of `manifest.proto`, capped at what [`crate::parse_signed_manifest`] accepts.
pub(super) async fn manifest(download: &GameDownload) -> Result<Vec<u8>, ManifestFetch> {
    let client = client().map_err(|err| ManifestFetch::Unreachable(err.to_string()))?;
    let network = |err: reqwest::Error| ManifestFetch::Unreachable(err.without_url().to_string());
    let mut response = client.get(download.manifest_url()).send().await.map_err(network)?;
    let status = response.status();
    if !status.is_success() {
        tracing::warn!(
            status = status.as_u16(),
            host = download.host(),
            "amazon refused the game manifest"
        );
        return Err(ManifestFetch::Unreachable(format!("HTTP {status} for the manifest")));
    }
    let declared = response
        .content_length()
        .and_then(|length| usize::try_from(length).ok());
    if declared.is_some_and(|length| length > MAX_RAW_LEN) {
        return Err(ManifestFetch::TooLarge);
    }
    let mut raw = Vec::with_capacity(declared.unwrap_or_default());
    while let Some(chunk) = response.chunk().await.map_err(network)? {
        if raw.len().saturating_add(chunk.len()) > MAX_RAW_LEN {
            return Err(ManifestFetch::TooLarge);
        }
        raw.extend_from_slice(&chunk);
    }
    Ok(raw)
}
