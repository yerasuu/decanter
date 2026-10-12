//! A port's build for its checkout's commit, published by the port's CI as a
//! Nix binary cache, so a commit upstream already built is fetched instead of
//! built on every machine.
//!
//! The cache is flat: `commit-<sha>` names that commit's output store path,
//! and is uploaded only after every path of its closure. CI publishes a
//! build for each `vX.Y.Z` tag, and `latest-release` names the newest one.
//! Its paths are content-addressed and unsigned, which nix accepts from a
//! user its daemon doesn't trust.

use std::path::PathBuf;
use std::time::Duration;

use crate::paths::Root;
use crate::port::Port;

/// Points every port's cache somewhere else, a local `http://` server or
/// `file://` folder holding a copy of one, to try a cache before CI
/// publishes it.
const CACHE_OVERRIDE: &str = "DECANTER_PS_BINARY_CACHE";

#[derive(Debug, thiserror::Error)]
pub enum LookupError {
    #[error("could not reach the binary cache: {0}")]
    Request(#[from] reqwest::Error),
    #[error("could not read the binary cache: {0}")]
    Read(#[from] std::io::Error),
    #[error("the binary cache names something other than a store path for this commit: {0:?}")]
    Malformed(String),
    #[error("the binary cache's latest-release is not `<tag> <commit> <store path>`: {0:?}")]
    MalformedRelease(String),
}

/// The tag CI last published a build of, as `latest-release` names it, each
/// part validated: they go into git arguments, a URL and nix's arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Release {
    pub tag: String,
    pub commit: String,
    pub store_path: String,
}

/// Where `port`'s CI publishes its builds, when it does.
pub fn cache(port: Port) -> Option<String> {
    port.binary_cache()
        .map(|published| std::env::var(CACHE_OVERRIDE).unwrap_or_else(|_| published.to_owned()))
}

/// The commit goes into a URL; only a full SHA-1 as git prints it gets there.
pub fn valid_commit(commit: &str) -> bool {
    commit.len() == 40 && commit.bytes().all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
}

/// Only the `vX.Y.Z` tags CI publishes builds of.
pub fn valid_tag(tag: &str) -> bool {
    let Some(version) = tag.strip_prefix('v') else {
        return false;
    };
    let mut parts = version.split('.');
    let mut number = || {
        parts
            .next()
            .is_some_and(|part| !part.is_empty() && part.len() <= 9 && part.bytes().all(|byte| byte.is_ascii_digit()))
    };
    number() && number() && number() && parts.next().is_none()
}

/// A store path goes into nix's arguments and a GC root, and comes from the
/// network or a marker on disk: `/nix/store/<hash>-<name>` with nix's own
/// alphabets, nothing below it.
pub fn valid_store_path(path: &str) -> bool {
    let Some((hash, name)) = path.strip_prefix("/nix/store/").and_then(|rest| rest.split_once('-')) else {
        return false;
    };
    hash.len() == 32
        && hash
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (byte.is_ascii_lowercase() && !b"eotu".contains(&byte)))
        && !name.is_empty()
        && !name.starts_with('.')
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"+-._?=".contains(&byte))
}

/// `name`'s contents in the cache, `None` when it has no such file.
async fn fetch(cache: &str, name: &str) -> Result<Option<String>, LookupError> {
    if let Some(dir) = cache.strip_prefix("file://") {
        return match tokio::fs::read_to_string(std::path::Path::new(dir).join(name)).await {
            Ok(text) => Ok(Some(text)),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(err) => Err(err.into()),
        };
    }
    // Bounded, since every preparation of the port waits on it before building.
    let client = match reqwest::Client::builder().timeout(Duration::from_secs(20)).build() {
        Ok(client) => client,
        Err(err) => return Err(err.into()),
    };
    let response = match client.get(format!("{cache}/{name}")).send().await {
        Ok(response) if response.status() == reqwest::StatusCode::NOT_FOUND => return Ok(None),
        Ok(response) => response.error_for_status(),
        Err(err) => Err(err),
    };
    let body = match response {
        Ok(found) => found.text().await,
        Err(err) => Err(err),
    };
    match body {
        Ok(text) => Ok(Some(text)),
        Err(err) => Err(err.into()),
    }
}

/// The output store path published for `commit`, or `None` when the cache
/// has none: not a commit CI builds, or its build not uploaded yet.
pub async fn lookup(cache: &str, commit: &str) -> Result<Option<String>, LookupError> {
    let text = match fetch(cache, &format!("commit-{commit}")).await {
        Ok(Some(text)) => text,
        Ok(None) => return Ok(None),
        Err(err) => return Err(err),
    };
    let path = text.trim();
    if valid_store_path(path) {
        Ok(Some(path.to_owned()))
    } else {
        Err(LookupError::Malformed(path.chars().take(120).collect()))
    }
}

/// The newest release CI published, or `None` before the first one.
pub async fn latest_release(cache: &str) -> Result<Option<Release>, LookupError> {
    let text = match fetch(cache, "latest-release").await {
        Ok(Some(text)) => text,
        Ok(None) => return Ok(None),
        Err(err) => return Err(err),
    };
    let line = text.trim();
    let mut parts = line.split_whitespace();
    match (parts.next(), parts.next(), parts.next(), parts.next()) {
        (Some(tag), Some(commit), Some(store_path), None)
            if valid_tag(tag) && valid_commit(commit) && valid_store_path(store_path) =>
        {
            Ok(Some(Release {
                tag: tag.to_owned(),
                commit: commit.to_owned(),
                store_path: store_path.to_owned(),
            }))
        }
        _ => Err(LookupError::MalformedRelease(line.chars().take(200).collect())),
    }
}

/// The symlink that keeps `port`'s fetched build from nix's garbage
/// collector, replaced by each fetch so the previous build can go.
pub fn gc_root(root: &Root, port: Port) -> PathBuf {
    root.toolchain().join("prebuilt").join(port.name().to_lowercase())
}
