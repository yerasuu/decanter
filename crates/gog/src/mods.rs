//! The fan-made mods GOG sells as separate free products, found through
//! its public catalog's `mod` tag ([GOG Mods](https://www.gog.com/en/mods)).
//!
//! Discovery only: adding one to an account happens on gog.com, because
//! GOG documents no endpoint for it. Once added it is an ordinary owned
//! game, so nothing here touches the library listing; the list only marks
//! which owned games are mods and offers the rest.
//!
//! Only the id, title and store link of each product are kept, checked
//! here whether they come from GOG or from the cache file, which sits in
//! the library folder where anything may have edited it.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use anyhow::Context as _;
use serde::{Deserialize, Serialize};

use crate::api::client::Sessionless;

const STORE_HOST: &str = "www.gog.com";
/// One page of the catalog: GOG listed 30 mods in 2026-10.
const MAX_MODS: usize = 100;
const MAX_ID_DIGITS: usize = 20;
const MAX_TITLE_CHARS: usize = 200;
const MAX_LINK_BYTES: usize = 512;
const MAX_CACHE_BYTES: usize = 4 * 1024 * 1024;
const CACHE_TTL_SECS: i64 = 24 * 60 * 60;
const CACHE_FILE: &str = "gog-mods.json";

/// A mod product as decanter keeps it: an id that matches the library's
/// product ids, a printable title and a gog.com page to add it from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GogMod {
    pub id: String,
    pub title: String,
    pub store_url: String,
}

#[derive(Serialize, Deserialize)]
struct CacheFile {
    fetched_at: i64,
    mods: Vec<GogMod>,
}

/// GOG's mods, from a cache younger than a day or else from the catalog.
///
/// `None` when neither has them: no active library, GOG unreachable or
/// answering something unexpected. Nothing else depends on the list, so
/// that only means the mods list says it is unavailable.
pub async fn gog_mods() -> Option<Vec<GogMod>> {
    let Some(path) = cache_path() else {
        tracing::info!("no active library, GOG mods list unavailable");
        return None;
    };
    let now = chrono::Utc::now().timestamp();
    if let Some(cached) = fresh_cache(&path, now).await {
        return Some(cached);
    }
    match fetch().await {
        Ok(mods) => {
            let cache = CacheFile { fetched_at: now, mods };
            write_cache(&path, &cache).await;
            Some(cache.mods)
        }
        Err(err) => {
            tracing::warn!(error = %format_args!("{err:#}"), "GOG mods catalog request failed, mods list unavailable");
            None
        }
    }
}

/// A cache stamped in the future counts as stale, so a clock set back
/// can't keep an old list alive.
async fn fresh_cache(path: &Path, now: i64) -> Option<Vec<GogMod>> {
    let cached = read_cache(path).await?;
    let fresh = (0..CACHE_TTL_SECS).contains(&now.saturating_sub(cached.fetched_at));
    tracing::debug!(fresh, count = cached.mods.len(), "GOG mods cache read");
    fresh.then_some(cached.mods)
}

fn cache_path() -> Option<PathBuf> {
    decanter_core::bootstrap::data_dir().map(|dir| dir.join(CACHE_FILE))
}

async fn fetch() -> anyhow::Result<Vec<GogMod>> {
    let client = Sessionless::new().context("building the GOG HTTP client")?;
    let page = client
        .catalog_mods(MAX_MODS)
        .await
        .context("requesting the GOG catalog")?;
    let mods = validated(
        page.products
            .into_iter()
            .filter_map(|product| checked(product.id, &product.title, &product.store_link)),
    );
    if mods.len() < page.listed {
        tracing::warn!(
            listed = page.listed,
            kept = mods.len(),
            "GOG catalog products dropped by validation"
        );
    }
    tracing::info!(count = mods.len(), "GOG mods catalog fetched");
    Ok(mods)
}

/// Deduplicated by id and capped, so a hostile or broken answer can't grow
/// the cache or the list without bound.
fn validated(mods: impl Iterator<Item = GogMod>) -> Vec<GogMod> {
    let mut seen = HashSet::new();
    mods.filter(|product| seen.insert(product.id.clone()))
        .take(MAX_MODS)
        .collect()
}

fn checked(id: String, raw_title: &str, store_link: &str) -> Option<GogMod> {
    let id_valid = !id.is_empty() && id.len() <= MAX_ID_DIGITS && id.bytes().all(|byte| byte.is_ascii_digit());
    if !id_valid {
        return None;
    }
    let title = printable_title(raw_title)?;
    let store_url = store_url(store_link)?;
    Some(GogMod { id, title, store_url })
}

/// Bidirectional overrides are format characters, not control ones, but
/// they reorder the text around them just as surely.
fn printable_title(raw: &str) -> Option<String> {
    let printable: String = raw
        .chars()
        .filter(|c| !c.is_control() && !matches!(c, '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}'))
        .collect();
    let title: String = printable.trim().chars().take(MAX_TITLE_CHARS).collect();
    (!title.is_empty()).then_some(title)
}

/// The link is handed to the desktop's opener, so it has to be a gog.com
/// page and nothing else.
fn store_url(raw: &str) -> Option<String> {
    if raw.len() > MAX_LINK_BYTES {
        return None;
    }
    let url = url::Url::parse(raw).ok()?;
    let on_store = url.scheme() == "https"
        && url.host_str() == Some(STORE_HOST)
        && url.port().is_none()
        && url.username().is_empty()
        && url.password().is_none();
    on_store.then(|| url.into())
}

async fn read_cache(path: &Path) -> Option<CacheFile> {
    let raw = match tokio::fs::read(path).await {
        Ok(raw) => raw,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return None,
        Err(err) => {
            tracing::warn!(path = %path.display(), %err, "cannot read the GOG mods cache");
            return None;
        }
    };
    if raw.len() > MAX_CACHE_BYTES {
        tracing::warn!(path = %path.display(), "GOG mods cache is too large, ignoring it");
        return None;
    }
    let cached: CacheFile = serde_json::from_slice(&raw)
        .inspect_err(|err| tracing::warn!(path = %path.display(), %err, "GOG mods cache is corrupt, ignoring it"))
        .ok()?;
    let mods = validated(
        cached
            .mods
            .into_iter()
            .filter_map(|product| checked(product.id, &product.title, &product.store_url)),
    );
    Some(CacheFile {
        fetched_at: cached.fetched_at,
        mods,
    })
}

/// Written beside its final name and renamed into place, so a crash
/// mid-write leaves the old cache or none, never half a file.
async fn write_cache(path: &Path, cache: &CacheFile) {
    let encoded = match serde_json::to_vec(cache) {
        Ok(encoded) => encoded,
        Err(err) => {
            tracing::warn!(%err, "cannot encode the GOG mods cache");
            return;
        }
    };
    let partial = path.with_extension("json.part");
    let written = match tokio::fs::write(&partial, encoded).await {
        Ok(()) => tokio::fs::rename(&partial, path).await,
        Err(err) => Err(err),
    };
    if let Err(err) = written {
        tracing::warn!(path = %path.display(), %err, "cannot write the GOG mods cache");
    }
}
