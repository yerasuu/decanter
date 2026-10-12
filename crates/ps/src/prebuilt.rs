//! A port's build for its checkout's commit, published by the port's CI as a
//! Nix binary cache, so a commit upstream already built is fetched instead of
//! built on every machine.
//!
//! The cache is flat: `commit-<sha>` names that commit's output store path,
//! and is uploaded only after every path of its closure. Its paths are
//! content-addressed and unsigned, which nix accepts from a user its daemon
//! doesn't trust.

use std::path::PathBuf;
use std::time::Duration;

use crate::paths::Root;
use crate::port::Port;

/// Points every port's cache somewhere else, a local `http://` server holding
/// a copy of one, to try a cache before CI publishes it.
const CACHE_OVERRIDE: &str = "DECANTER_PS_BINARY_CACHE";

#[derive(Debug, thiserror::Error)]
pub enum LookupError {
    #[error("could not reach the binary cache: {0}")]
    Request(#[from] reqwest::Error),
    #[error("the binary cache names something other than a store path for this commit: {0:?}")]
    Malformed(String),
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

/// The output store path published for `commit`, or `None` when the cache
/// has none: not a commit CI builds, or its build not uploaded yet.
pub async fn lookup(cache: &str, commit: &str) -> Result<Option<String>, LookupError> {
    // Bounded, since every preparation of the port waits on it before building.
    let client = reqwest::Client::builder().timeout(Duration::from_secs(20)).build()?;
    let response = match client.get(format!("{cache}/commit-{commit}")).send().await {
        Ok(response) if response.status() == reqwest::StatusCode::NOT_FOUND => return Ok(None),
        Ok(response) => response.error_for_status(),
        Err(err) => Err(err),
    };
    let body = match response {
        Ok(found) => found.text().await,
        Err(err) => Err(err),
    };
    let text = match body {
        Ok(text) => text,
        Err(err) => return Err(err.into()),
    };
    let path = text.trim();
    if valid_store_path(path) {
        Ok(Some(path.to_owned()))
    } else {
        Err(LookupError::Malformed(path.chars().take(120).collect()))
    }
}

/// The symlink that keeps `port`'s fetched build from nix's garbage
/// collector, replaced by each fetch so the previous build can go.
pub fn gc_root(root: &Root, port: Port) -> PathBuf {
    root.toolchain().join("prebuilt").join(port.name().to_lowercase())
}
