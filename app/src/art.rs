//! GOG's cover, icon and background art, fetched once and kept on disk.
//!
//! The webview build pointed `<img src>` straight at GOG's CDN and let
//! WebKit's HTTP cache deal with it. GPUI's own URI loader keeps nothing
//! between launches, so a 700-game library would re-download every icon on
//! every start; this keeps a copy under decanter's data directory instead
//! and hands GPUI a plain file path.

use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, OnceLock};

use gpui::{App, Asset, ImageAssetLoader, ImageCacheError, ImageSource, Resource, SharedString, Window};

static TOKIO: OnceLock<tokio::runtime::Handle> = OnceLock::new();
static CLIENT: LazyLock<reqwest::Client> = LazyLock::new(reqwest::Client::new);

/// GPUI loads assets on its own background threads, outside the tokio
/// context `main` enters on the UI thread — so downloads are handed to the
/// runtime explicitly.
pub fn init(handle: tokio::runtime::Handle) {
    let _ = TOKIO.set(handle);
}

fn cache_dir() -> PathBuf {
    decanter_core::data_dir().join("art-cache")
}

/// The file a URL is cached as: its md5, keeping the extension so the
/// directory stays browsable.
fn cache_path(url: &str) -> PathBuf {
    let extension = Path::new(url.split('?').next().unwrap_or(url))
        .extension()
        .and_then(|extension| extension.to_str())
        .filter(|extension| extension.len() <= 4)
        .unwrap_or("img");
    cache_dir().join(format!("{:x}.{extension}", md5::compute(url)))
}

async fn download(url: String, path: PathBuf) -> anyhow::Result<()> {
    let response = match CLIENT.get(&url).send().await {
        Ok(response) => response,
        Err(err) => return Err(anyhow::anyhow!("requesting {url}: {err}")),
    };
    let response = match response.error_for_status() {
        Ok(response) => response,
        Err(err) => return Err(anyhow::anyhow!("fetching {url}: {err}")),
    };
    let bytes = match response.bytes().await {
        Ok(bytes) => bytes,
        Err(err) => return Err(anyhow::anyhow!("reading {url}: {err}")),
    };
    if let Err(err) = tokio::fs::create_dir_all(cache_dir()).await {
        return Err(anyhow::anyhow!("creating the art cache: {err}"));
    }
    // Written aside and renamed into place, so a half-written file never
    // passes for a cached one after a crash.
    let partial = path.with_extension("part");
    if let Err(err) = tokio::fs::write(&partial, &bytes).await {
        return Err(anyhow::anyhow!("writing {}: {err}", partial.display()));
    }
    if let Err(err) = tokio::fs::rename(&partial, &path).await {
        return Err(anyhow::anyhow!("moving {} into place: {err}", path.display()));
    }
    Ok(())
}

enum CachedArt {}

impl Asset for CachedArt {
    type Source = SharedString;
    type Output = Result<Arc<Path>, Arc<anyhow::Error>>;

    fn load(url: Self::Source, _: &mut App) -> impl Future<Output = Self::Output> + Send + 'static {
        async move {
            let path = cache_path(&url);
            if path.is_file() {
                return Ok(Arc::from(path));
            }
            let Some(tokio) = TOKIO.get() else {
                return Err(Arc::new(anyhow::anyhow!("art cache used before init")));
            };
            match tokio.spawn(download(url.to_string(), path.clone())).await {
                Ok(Ok(())) => Ok(Arc::from(path)),
                Ok(Err(err)) => {
                    tracing::debug!(%url, %err, "could not fetch artwork");
                    Err(Arc::new(err))
                }
                Err(err) => Err(Arc::new(anyhow::anyhow!("artwork download task failed: {err}"))),
            }
        }
    }
}

/// An image source for a remote artwork URL; the view re-renders once it
/// has arrived. Pair with `img(..).with_fallback(..)` for the placeholder.
pub fn source(url: &str) -> ImageSource {
    let url = SharedString::from(url.to_owned());
    ImageSource::Custom(Arc::new(move |window: &mut Window, cx: &mut App| {
        match window.use_asset::<CachedArt>(&url, cx)? {
            Ok(path) => window.use_asset::<ImageAssetLoader>(&Resource::Path(path), cx),
            Err(err) => Some(Err(ImageCacheError::Other(err))),
        }
    }))
}
