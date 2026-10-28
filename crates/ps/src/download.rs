//! Fetching what a port's CI publishes: bounded downloads checked against their published SHA-256 and size.
//!
//! Shared by the Linux bundles ([`crate::bundle`]), the Windows package ([`crate::package`]) and the
//! Windows toolchain ([`crate::winbuild`]), so neither system's code depends on the other's.

use std::io;
use std::path::Path;
use std::time::Duration;

use decanter_core::capabilities::Feature;
use decanter_core::platform;
pub(crate) use decanter_runtime::download::hex_digest;
use decanter_runtime::download::{self as fetched, Expected};
use sha2::{Digest as _, Sha256};

use crate::port::Port;

/// Points every port's bundles somewhere else, a local `http://` server or a `file://` folder
/// laid out as `<tag>/<asset>`, to try a bundle before CI publishes it.
const DOWNLOADS_OVERRIDE: &str = "DECANTER_PS_BUNDLES";

/// About 50 MB today: the cap only stops a wrong or runaway download.
pub(crate) const MAX_BYTES: u64 = 512 << 20;

pub(crate) const MAX_CHECKSUM_BYTES: usize = 4096;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("could not download the bundle: {0}")]
    Download(#[from] reqwest::Error),
    #[error("the bundle is larger than the {MAX_BYTES}-byte cap: {0} bytes")]
    TooLarge(u64),
    #[error("the bundle's checksum file is not `<sha256>  {expected}`: {found:?}")]
    MalformedChecksum { expected: String, found: String },
    #[error("the bundle does not match its published checksum")]
    Checksum,
    #[error("the download is {found} bytes, not the {expected} published")]
    Size { expected: u64, found: u64 },
    #[error("tar could not extract the bundle: {0}")]
    Extract(String),
    #[error("the bundle holds {0}")]
    Layout(String),
    #[error("the bundle could not be pointed at its own loader: {0}")]
    Relocate(String),
    #[error("{0}")]
    Io(#[from] io::Error),
}

/// Where the published bundles of `port` are, when its CI publishes any for this machine.
pub(crate) fn downloads(port: Port) -> Option<String> {
    if !platform::support(Feature::PsDownloads).is_offered() {
        return None;
    }
    port.release_downloads()
        .map(|published| std::env::var(DOWNLOADS_OVERRIDE).unwrap_or_else(|_| published.to_owned()))
}

/// The file at `url`, `None` when there is none, and an error past `cap` bytes, following the
/// redirects `redirect` allows.
pub(crate) async fn fetch_small(
    url: &str,
    cap: usize,
    redirect: reqwest::redirect::Policy,
) -> Result<Option<Vec<u8>>, Error> {
    if let Some(path) = url.strip_prefix("file://") {
        return match tokio::fs::read(path).await {
            Ok(bytes) if bytes.len() > cap => Err(Error::TooLarge(bytes.len() as u64)),
            Ok(bytes) => Ok(Some(bytes)),
            Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(err) => Err(err.into()),
        };
    }
    // Bounded, since every preparation of the port on Release waits on it.
    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(20))
        .redirect(redirect)
        .build()
    {
        Ok(client) => client,
        Err(err) => return Err(err.into()),
    };
    let response = match client.get(url).send().await {
        Ok(response) if response.status() == reqwest::StatusCode::NOT_FOUND => return Ok(None),
        Ok(response) => response.error_for_status(),
        Err(err) => Err(err),
    };
    let bytes = match response {
        Ok(response) => response.bytes().await,
        Err(err) => Err(err),
    };
    match bytes {
        Ok(bytes) if bytes.len() > cap => Err(Error::TooLarge(bytes.len() as u64)),
        Ok(bytes) => Ok(Some(bytes.to_vec())),
        Err(err) => Err(err.into()),
    }
}

/// The SHA-256 a `sha256sum` line gives for `asset`: `<sha256>  <asset>`, nothing else.
pub(crate) fn checksum_of(text: &str, asset: &str) -> Option<String> {
    let mut parts = text.split_whitespace();
    match (parts.next(), parts.next(), parts.next()) {
        (Some(sum), Some(name), None)
            if sum.len() == 64
                && sum.bytes().all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
                && name.strip_prefix('*').unwrap_or(name) == asset =>
        {
            Some(sum.to_owned())
        }
        _ => None,
    }
}

/// `size`, when the publisher gives one, is checked as the bytes arrive, before the checksum.
pub(crate) async fn download(
    url: &str,
    archive: &Path,
    sha256: &str,
    size: Option<u64>,
    redirect: reqwest::redirect::Policy,
    mut on_download: impl FnMut(u64, Option<u64>),
) -> Result<(), Error> {
    if let Some(path) = url.strip_prefix("file://") {
        let found = match tokio::fs::metadata(path).await {
            Ok(meta) => meta.len(),
            Err(err) => return Err(err.into()),
        };
        if found > MAX_BYTES {
            return Err(Error::TooLarge(found));
        }
        if let Some(expected) = size.filter(|&expected| expected != found) {
            return Err(Error::Size { expected, found });
        }
        let size = found;
        let bytes = match tokio::fs::read(path).await {
            Ok(bytes) => bytes,
            Err(err) => return Err(err.into()),
        };
        on_download(size, Some(size));
        if hex_digest(&Sha256::digest(&bytes)) != sha256 {
            return Err(Error::Checksum);
        }
        return match tokio::fs::write(archive, &bytes).await {
            Ok(()) => Ok(()),
            Err(err) => Err(err.into()),
        };
    }
    let pin = Expected {
        digest: sha256,
        size,
        cap: MAX_BYTES,
    };
    match fetched::fetch::<Sha256>(url, archive, &pin, redirect, on_download).await {
        Ok(()) => Ok(()),
        Err(fetched::Error::Request(err)) => Err(Error::Download(err)),
        Err(fetched::Error::TooLarge { found, .. }) => Err(Error::TooLarge(found)),
        Err(fetched::Error::Size { expected, found }) => Err(Error::Size { expected, found }),
        Err(fetched::Error::Checksum) => Err(Error::Checksum),
        Err(fetched::Error::Io(err)) => Err(Error::Io(err)),
    }
}
