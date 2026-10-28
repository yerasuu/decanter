//! The newest release a GitHub repository published, asked only when the
//! user presses a check button: decanter itself, and for the runners
//! ([`crate::runners::latest`]) and nix-portable, the repositories they come
//! from. Every part of GitHub's answer is validated here before it is shown
//! or opened.

use std::borrow::Cow;
use std::cmp::Ordering;
use std::time::Duration;

use rust_i18n::t;
use serde::Deserialize;

/// Where decanter's own releases are published, by the CI on every `vX.Y.Z` tag.
pub const DECANTER_REPO: &str = "yerasuu/decanter";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Release {
    pub tag: String,
    /// The release's page on GitHub, with its notes and assets.
    pub url: String,
}

#[derive(Debug, Clone, thiserror::Error)]
pub enum ReleaseError {
    #[error("no connection to GitHub: {0}")]
    Offline(String),
    #[error("GitHub's rate limit for anonymous requests was reached")]
    RateLimited,
    #[error("GitHub answered something that isn't a valid release: {0}")]
    Invalid(String),
    #[error("GitHub request failed: {0}")]
    Failed(String),
}

impl ReleaseError {
    pub fn localized(&self) -> Cow<'static, str> {
        match self {
            Self::Offline(_) => t!("settings.updates.errors.offline"),
            Self::RateLimited => t!("settings.updates.errors.rate_limited"),
            Self::Invalid(detail) => t!("settings.updates.errors.invalid", error = detail),
            Self::Failed(detail) => t!("settings.updates.errors.failed", error = detail),
        }
    }
}

impl From<reqwest::Error> for ReleaseError {
    fn from(err: reqwest::Error) -> Self {
        if err.is_connect() || err.is_timeout() {
            Self::Offline(err.to_string())
        } else if err.is_decode() {
            Self::Invalid(err.to_string())
        } else {
            Self::Failed(err.to_string())
        }
    }
}

#[derive(Deserialize)]
struct Answer {
    tag_name: String,
    html_url: String,
}

/// Tags go into the UI and into a path under Steam's compat folder: plain
/// release names only.
fn valid_tag(tag: &str) -> bool {
    (1..=64).contains(&tag.len())
        && !tag.starts_with(['-', '.'])
        && tag
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_' | b'+'))
}

/// The page is handed to the desktop's opener: only a release page on
/// github.com, of any repository, since a renamed one redirects.
fn valid_page(url: &str, tag: &str) -> bool {
    url.strip_prefix("https://github.com/")
        .and_then(|rest| rest.strip_suffix(tag))
        .and_then(|rest| rest.strip_suffix("/releases/tag/"))
        .and_then(|repo| repo.split_once('/'))
        .is_some_and(|(owner, name)| {
            [owner, name].iter().all(|part| {
                !part.is_empty()
                    && part
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
            })
        })
}

/// The newest non-draft, non-prerelease release of `repo` (`owner/name`),
/// asked of GitHub's API every call, or `None` when it has published none.
pub async fn latest(repo: &str) -> Result<Option<Release>, ReleaseError> {
    let client = crate::achv_try!(
        reqwest::Client::builder()
            .timeout(Duration::from_secs(20))
            .user_agent(concat!("decanter/", env!("CARGO_PKG_VERSION")))
            .build()
    );
    let response = crate::achv_try!(
        client
            .get(format!("https://api.github.com/repos/{repo}/releases/latest"))
            .header(reqwest::header::ACCEPT, "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28")
            .header(reqwest::header::CACHE_CONTROL, "no-cache")
            .send()
            .await
    );
    let status = response.status();
    let exhausted = response
        .headers()
        .get("x-ratelimit-remaining")
        .is_some_and(|remaining| remaining.as_bytes() == b"0");
    if status == reqwest::StatusCode::NOT_FOUND {
        tracing::info!(repo, "no published release");
        return Ok(None);
    }
    if status == reqwest::StatusCode::TOO_MANY_REQUESTS || (status == reqwest::StatusCode::FORBIDDEN && exhausted) {
        tracing::warn!(repo, %status, "GitHub rate limit reached");
        return Err(ReleaseError::RateLimited);
    }
    let answer: Answer = crate::achv_try!(crate::achv_try!(response.error_for_status()).json().await);
    if !valid_tag(&answer.tag_name) || !valid_page(&answer.html_url, &answer.tag_name) {
        tracing::warn!(
            repo,
            tag = answer.tag_name,
            url = answer.html_url,
            "rejected GitHub release answer"
        );
        return Err(ReleaseError::Invalid(answer.tag_name.chars().take(64).collect()));
    }
    tracing::debug!(repo, tag = answer.tag_name, "latest release");
    Ok(Some(Release {
        tag: answer.tag_name,
        url: answer.html_url,
    }))
}

/// Where this build stands against decanter's latest release.
#[derive(Debug, Clone)]
pub enum Standing {
    UpToDate(Release),
    Newer(Release),
    /// A build from after the latest release, such as one from `develop`.
    Ahead(Release),
    NoRelease,
}

/// `MAJOR.MINOR.PATCH`, the shape `CARGO_PKG_VERSION` and decanter's `v` tags share.
pub fn version(text: &str) -> Option<(u64, u64, u64)> {
    let mut parts = text.split('.').map(|part| {
        (!part.is_empty() && part.len() <= 9 && part.bytes().all(|byte| byte.is_ascii_digit()))
            .then(|| part.parse().ok())
            .flatten()
    });
    match (parts.next(), parts.next(), parts.next(), parts.next()) {
        (Some(Some(major)), Some(Some(minor)), Some(Some(patch)), None) => Some((major, minor, patch)),
        _ => None,
    }
}

/// Compares this build with decanter's latest release, whose tag must be `vX.Y.Z`.
pub async fn decanter() -> Result<Standing, ReleaseError> {
    let Some(release) = crate::achv_try!(latest(DECANTER_REPO).await) else {
        return Ok(Standing::NoRelease);
    };
    let Some(published) = release.tag.strip_prefix('v').and_then(version) else {
        tracing::warn!(tag = release.tag, "latest decanter release is not a vX.Y.Z tag");
        return Err(ReleaseError::Invalid(release.tag));
    };
    let Some(running) = version(env!("CARGO_PKG_VERSION")) else {
        return Err(ReleaseError::Invalid(env!("CARGO_PKG_VERSION").to_owned()));
    };
    tracing::info!(
        running = env!("CARGO_PKG_VERSION"),
        latest = release.tag,
        "checked decanter's latest release"
    );
    Ok(match published.cmp(&running) {
        Ordering::Greater => Standing::Newer(release),
        Ordering::Equal => Standing::UpToDate(release),
        Ordering::Less => Standing::Ahead(release),
    })
}
