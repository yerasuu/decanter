//! GOG's cover, icon and background art, fetched once and kept on disk at
//! the size it's drawn at — and a PS dump's own icon, read from the dump
//! the same way through a `file://` URL.
//!
//! The webview build pointed `<img src>` straight at GOG's CDN and let
//! WebKit's HTTP cache deal with it. GPUI's own URI loader keeps nothing
//! between launches, so a 700-game library would re-download every icon on
//! every start; this keeps a copy under decanter's data directory instead
//! and hands GPUI a plain file path.
//!
//! What GPUI holds in memory is the decoded bitmap, so the copy on disk is
//! scaled down to the box it fills on screen first: a 26 px sidebar icon
//! used to decode from a 392×220 source, 30 times the pixels it needed.
//! GPUI's asset cache also never lets go of anything, so decoded images
//! are tracked here and the least recently drawn are dropped once
//! [`DECODED_BUDGET`] is passed.

use std::collections::HashMap;
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, OnceLock};
use std::time::{Duration, Instant};

use gpui::{
    App, Asset, Global, ImageAssetLoader, ImageCacheError, ImageSource, RenderImage, Resource, SharedString, Size,
    Window,
};
use image::{DynamicImage, ImageFormat, imageops::FilterType};

static TOKIO: OnceLock<tokio::runtime::Handle> = OnceLock::new();
static CLIENT: LazyLock<reqwest::Client> = LazyLock::new(reqwest::Client::new);

/// Decoded bytes kept across every window before the least recently drawn
/// image is dropped. A full screen of shelves plus a detail hero is well
/// under half of this, so nothing on screen is ever the one evicted.
const DECODED_BUDGET: usize = 24 * 1024 * 1024;

/// GPUI loads assets on its own background threads, outside the tokio
/// context `main` enters on the UI thread — so downloads are handed to the
/// runtime explicitly.
pub fn init(handle: tokio::runtime::Handle) {
    // Full-size copies from before art was stored scaled; nothing reads
    // them any more.
    handle.spawn(async {
        let legacy = decanter_core::data_dir().join("art-cache");
        match tokio::fs::remove_dir_all(&legacy).await {
            Ok(()) => tracing::info!(dir = %legacy.display(), "removed the full-size art cache"),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => tracing::warn!(dir = %legacy.display(), %err, "could not remove the full-size art cache"),
        }
    });
    let _ = TOKIO.set(handle);
}

fn cache_dir() -> PathBuf {
    decanter_core::data_dir().join("art")
}

/// One URL at one on-screen size, in device pixels — the same art drawn
/// in two places (a cover on its shelf and as a detail page's fallback
/// hero) is two files.
#[derive(Clone, PartialEq, Eq, Hash)]
struct ArtKey {
    url: SharedString,
    width: u32,
    height: u32,
}

/// No extension: what's stored may be GOG's WebP as downloaded or a
/// re-encoded JPEG/PNG, and GPUI sniffs the format from the bytes anyway.
fn cache_path(key: &ArtKey) -> PathBuf {
    cache_dir().join(format!(
        "{:x}-{}x{}",
        md5::compute(key.url.as_bytes()),
        key.width,
        key.height
    ))
}

/// GOG's image CDNs (`images*.gog-statics.com`, and `images.gog.com` for
/// GamesDB's art) serve every image as WebP too, at a half to two thirds
/// of the bytes of the JPEG the APIs link to.
fn webp_variant(url: &str) -> Option<String> {
    if !url.contains(".gog-statics.com/") && !url.starts_with("https://images.gog.com/") {
        return None;
    }
    let (path, query) = url
        .split_once('?')
        .map_or((url, None), |(path, query)| (path, Some(query)));
    let stem = path.strip_suffix(".jpg").or_else(|| path.strip_suffix(".png"));
    stem.map(|stem| match query {
        Some(query) => format!("{stem}.webp?{query}"),
        None => format!("{stem}.webp"),
    })
}

async fn get(url: &str) -> anyhow::Result<Vec<u8>> {
    let response = match CLIENT.get(url).send().await {
        Ok(response) => response,
        Err(err) => return Err(anyhow::anyhow!("requesting {url}: {err}")),
    };
    let response = match response.error_for_status() {
        Ok(response) => response,
        Err(err) => return Err(anyhow::anyhow!("fetching {url}: {err}")),
    };
    match response.bytes().await {
        Ok(bytes) => Ok(Vec::from(bytes)),
        Err(err) => Err(anyhow::anyhow!("reading {url}: {err}")),
    }
}

/// Scales `bytes` down until it just covers `width`×`height` — GPUI draws
/// art with `ObjectFit::Cover`, so the shorter side is the one that has to
/// fill the box. Art that's already no bigger is kept byte for byte.
fn fit(bytes: Vec<u8>, width: u32, height: u32) -> anyhow::Result<Vec<u8>> {
    let image = match image::load_from_memory(&bytes) {
        Ok(image) => image,
        Err(err) => return Err(anyhow::anyhow!("decoding: {err}")),
    };
    let scale = f64::max(
        f64::from(width) / f64::from(image.width()),
        f64::from(height) / f64::from(image.height()),
    );
    if scale >= 1. {
        return Ok(bytes);
    }
    let resized = image.resize_exact(
        (f64::from(image.width()) * scale).ceil() as u32,
        (f64::from(image.height()) * scale).ceil() as u32,
        FilterType::Triangle,
    );
    let mut encoded = Cursor::new(Vec::new());
    // JPEG has no alpha channel; the odd transparent icon stays PNG.
    let written = if resized.color().has_alpha() {
        resized.write_to(&mut encoded, ImageFormat::Png)
    } else {
        DynamicImage::ImageRgb8(resized.into_rgb8())
            .write_with_encoder(image::codecs::jpeg::JpegEncoder::new_with_quality(&mut encoded, 85))
    };
    match written {
        Ok(()) => Ok(encoded.into_inner()),
        Err(err) => Err(anyhow::anyhow!("re-encoding: {err}")),
    }
}

async fn download(key: ArtKey, path: PathBuf) -> anyhow::Result<()> {
    let bytes = crate::achv_try!(read(&key.url).await);
    crate::achv_try!(store(&key, &path, bytes).await);
    Ok(())
}

async fn read(url: &str) -> anyhow::Result<Vec<u8>> {
    match url.strip_prefix("file://") {
        Some(local) => tokio::fs::read(local)
            .await
            .map_err(|err| anyhow::anyhow!("reading {local}: {err}")),
        None => fetch(url).await,
    }
}

/// Square side a game's task bar icon is stored at, at most: what a
/// desktop draws icons at, with room for a large panel at double scale.
const DESKTOP_ICON: u32 = 256;

/// Writes `url`'s art to `dest` as a PNG, scaled down to
/// [`DESKTOP_ICON`] when bigger: a desktop entry's icon has to be a format
/// every desktop reads, which GOG's JPEG and WEBP art isn't.
pub async fn save_png(url: &str, dest: &Path) -> anyhow::Result<()> {
    let bytes = crate::achv_try!(read(url).await);
    let png = tokio::task::spawn_blocking(move || {
        let image = image::load_from_memory(&bytes).map_err(|err| anyhow::anyhow!("decoding: {err}"))?;
        let image = if image.width() > DESKTOP_ICON || image.height() > DESKTOP_ICON {
            image.thumbnail(DESKTOP_ICON, DESKTOP_ICON)
        } else {
            image
        };
        let mut encoded = Cursor::new(Vec::new());
        image
            .write_to(&mut encoded, ImageFormat::Png)
            .map_err(|err| anyhow::anyhow!("encoding: {err}"))?;
        anyhow::Ok(encoded.into_inner())
    });
    let png = match png.await {
        Ok(Ok(png)) => png,
        Ok(Err(err)) => return Err(anyhow::anyhow!("{url}: {err}")),
        Err(err) => return Err(anyhow::anyhow!("converting {url}: {err}")),
    };
    if let Some(dir) = dest.parent() {
        crate::achv_try!(tokio::fs::create_dir_all(dir).await);
    }
    let partial = dest.with_extension("part");
    crate::achv_try!(tokio::fs::write(&partial, &png).await);
    crate::achv_try!(tokio::fs::rename(&partial, dest).await);
    Ok(())
}

async fn fetch(url: &str) -> anyhow::Result<Vec<u8>> {
    let bytes = match webp_variant(url) {
        Some(webp) => match get(&webp).await {
            Ok(bytes) => bytes,
            Err(err) => {
                tracing::debug!(url, %err, "no webp variant, fetching the original");
                crate::achv_try!(get(url).await)
            }
        },
        None => crate::achv_try!(get(url).await),
    };
    Ok(bytes)
}

async fn store(key: &ArtKey, path: &Path, bytes: Vec<u8>) -> anyhow::Result<()> {
    // Decoding and resampling is CPU work; the runtime only has two
    // workers, and they're for I/O.
    let (width, height) = (key.width, key.height);
    let fitted = match tokio::task::spawn_blocking(move || fit(bytes, width, height)).await {
        Ok(Ok(fitted)) => fitted,
        Ok(Err(err)) => return Err(anyhow::anyhow!("{}: {err}", key.url)),
        Err(err) => return Err(anyhow::anyhow!("scaling {}: {err}", key.url)),
    };
    if let Err(err) = tokio::fs::create_dir_all(cache_dir()).await {
        return Err(anyhow::anyhow!("creating the art cache: {err}"));
    }
    // Written aside and renamed into place, so a half-written file never
    // passes for a cached one after a crash.
    let partial = path.with_extension("part");
    if let Err(err) = tokio::fs::write(&partial, &fitted).await {
        return Err(anyhow::anyhow!("writing {}: {err}", partial.display()));
    }
    if let Err(err) = tokio::fs::rename(&partial, path).await {
        return Err(anyhow::anyhow!("moving {} into place: {err}", path.display()));
    }
    Ok(())
}

enum CachedArt {}

impl Asset for CachedArt {
    type Source = ArtKey;
    type Output = Result<Arc<Path>, Arc<anyhow::Error>>;

    fn load(key: Self::Source, _: &mut App) -> impl Future<Output = Self::Output> + Send + 'static {
        async move {
            let path = cache_path(&key);
            if path.is_file() {
                return Ok(Arc::from(path));
            }
            let Some(tokio) = TOKIO.get() else {
                return Err(Arc::new(anyhow::anyhow!("art cache used before init")));
            };
            let url = key.url.clone();
            match tokio.spawn(download(key, path.clone())).await {
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

/// Every decoded image [`source`] has handed out, by when it was last
/// drawn — the eviction GPUI's asset cache doesn't do on its own.
#[derive(Default)]
struct Decoded {
    images: HashMap<Arc<Path>, (Arc<RenderImage>, Instant)>,
    bytes: usize,
    eviction_scheduled: bool,
}

impl Global for Decoded {}

/// Art drawn this recently is never evicted, however far over budget:
/// everything on screen is redrawn every frame, so this keeps it all, and
/// a screen that needs more than the budget just runs over it for a while
/// rather than dropping and reloading its own images in a loop.
const IN_VIEW: Duration = Duration::from_millis(500);

fn decoded_bytes(image: &RenderImage) -> usize {
    (0..image.frame_count())
        .map(|frame| image.as_bytes(frame).map_or(0, <[u8]>::len))
        .sum()
}

fn remember(path: &Arc<Path>, image: &Arc<RenderImage>, window: &mut Window, cx: &mut App) {
    let decoded = cx.default_global::<Decoded>();
    let now = Instant::now();
    if let Some((_, drawn)) = decoded.images.get_mut(path) {
        *drawn = now;
        return;
    }
    decoded.bytes = decoded.bytes.saturating_add(decoded_bytes(image));
    decoded.images.insert(Arc::clone(path), (Arc::clone(image), now));
    // Deferred to the next frame: images later in this one haven't been
    // marked drawn yet, and would look like the oldest.
    if decoded.bytes > DECODED_BUDGET && !decoded.eviction_scheduled {
        decoded.eviction_scheduled = true;
        window.on_next_frame(evict);
    }
}

fn evict(window: &mut Window, cx: &mut App) {
    let decoded = cx.default_global::<Decoded>();
    decoded.eviction_scheduled = false;
    let mut evicted = Vec::new();
    while decoded.bytes > DECODED_BUDGET {
        let Some(oldest) = decoded
            .images
            .iter()
            .filter(|(_, (_, drawn))| drawn.elapsed() >= IN_VIEW)
            .min_by_key(|(_, (_, drawn))| *drawn)
            .map(|(candidate, _)| Arc::clone(candidate))
        else {
            break;
        };
        if let Some((image, _)) = decoded.images.remove(&oldest) {
            decoded.bytes = decoded.bytes.saturating_sub(decoded_bytes(&image));
            evicted.push((oldest, image));
        }
    }
    for (path, image) in evicted {
        cx.remove_asset::<ImageAssetLoader>(&Resource::Path(path));
        cx.drop_image(image, Some(window));
    }
}

/// An image source for a remote artwork URL, stored and decoded at `size`
/// (logical pixels — the box the image is laid out in); the view
/// re-renders once it has arrived. Pair with `img(..).with_fallback(..)`
/// for the placeholder.
pub fn source(url: &str, size: Size<f32>) -> ImageSource {
    let url = SharedString::from(url.to_owned());
    ImageSource::Custom(Arc::new(move |window: &mut Window, cx: &mut App| {
        let scale = window.scale_factor();
        let key = ArtKey {
            url: url.clone(),
            width: (size.width * scale).ceil() as u32,
            height: (size.height * scale).ceil() as u32,
        };
        match window.use_asset::<CachedArt>(&key, cx)? {
            Ok(path) => {
                let loaded = window.use_asset::<ImageAssetLoader>(&Resource::Path(Arc::clone(&path)), cx);
                if let Some(Ok(image)) = &loaded {
                    remember(&path, image, window, cx);
                }
                loaded
            }
            Err(err) => Some(Err(ImageCacheError::Other(err))),
        }
    }))
}
