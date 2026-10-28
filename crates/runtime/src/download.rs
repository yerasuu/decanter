//! Downloads checked against a pin as they arrive: a byte cap, the published size when there is
//! one, and the digest once complete. Shared by the runners this crate installs and by psport's
//! downloads (`decanter-ps`).

use std::fmt::Write as _;
use std::io;
use std::path::Path;
use std::time::Duration;

use sha2::Digest;
use tokio::io::AsyncWriteExt as _;

/// Where `GitHub` serves release assets from, after `github.com` redirects there.
const RELEASE_HOSTS: [&str; 3] = [
    "github.com",
    "objects.githubusercontent.com",
    "release-assets.githubusercontent.com",
];

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{0}")]
    Request(#[from] reqwest::Error),
    #[error("the download is larger than the {cap}-byte cap: {found} bytes")]
    TooLarge { cap: u64, found: u64 },
    #[error("the download is {found} bytes, not the {expected} published")]
    Size { expected: u64, found: u64 },
    #[error("the download does not match its pinned checksum")]
    Checksum,
    #[error("{0}")]
    Io(#[from] io::Error),
}

/// What a download must turn out to be.
pub struct Expected<'pin> {
    /// Lower-case hex of the digest `D` of [`fetch`] gives the whole file.
    pub digest: &'pin str,
    pub size: Option<u64>,
    /// Bytes past which the download stops, whatever the server announces.
    pub cap: u64,
}

/// Lower-case hex of a digest, the form checksum files and pins carry.
#[must_use]
#[inline]
pub fn hex_digest(digest: &[u8]) -> String {
    digest
        .iter()
        .fold(String::with_capacity(digest.len() * 2), |mut hex, byte| {
            let _ = write!(hex, "{byte:02x}");
            hex
        })
}

/// Whether `text` is a digest of `bytes` bytes as [`hex_digest`] writes it.
#[must_use]
#[inline]
pub fn is_hex_digest(text: &str, bytes: usize) -> bool {
    text.len() == bytes * 2 && text.bytes().all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
}

/// Redirects only to `GitHub`'s release storage over HTTPS, at most five: a release download sent
/// anywhere else fails rather than fetching from there.
#[must_use]
#[inline]
pub fn github_releases() -> reqwest::redirect::Policy {
    reqwest::redirect::Policy::custom(|attempt| {
        let allowed = attempt.url().scheme() == "https"
            && attempt
                .url()
                .host_str()
                .is_some_and(|host| RELEASE_HOSTS.contains(&host));
        if attempt.previous().len() >= 5 {
            attempt.error("too many redirects")
        } else if allowed {
            attempt.follow()
        } else {
            tracing::warn!(url = %attempt.url(), "release download redirected off GitHub");
            attempt.error("redirected off GitHub's release storage")
        }
    })
}

/// Streams `url` into `file`, checking the cap and the size as the bytes arrive and the digest `D`
/// at the end. On an error `file` may hold part of the download: the caller removes it.
///
/// # Errors
/// When the request fails, the download outgrows the cap or the size, its digest differs, or
/// `file` can't be written.
#[inline]
pub async fn fetch<D: Digest>(
    url: &str,
    file: &Path,
    expected: &Expected<'_>,
    redirect: reqwest::redirect::Policy,
    mut on_download: impl FnMut(u64, Option<u64>),
) -> Result<(), Error> {
    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(20))
        .read_timeout(Duration::from_secs(60))
        .redirect(redirect)
        .build()?;
    let mut response = client
        .get(url)
        .send()
        .await
        .and_then(reqwest::Response::error_for_status)?;
    let total = response.content_length();
    if let Some(found) = total.filter(|&total| total > expected.cap) {
        return Err(Error::TooLarge {
            cap: expected.cap,
            found,
        });
    }
    if let (Some(size), Some(found)) = (expected.size, total)
        && size != found
    {
        return Err(Error::Size { expected: size, found });
    }
    let mut writer = tokio::fs::File::create(file).await?;
    let mut hasher = D::new();
    let mut received = 0;
    while let Some(chunk) = response.chunk().await? {
        received += chunk.len() as u64;
        if received > expected.cap {
            return Err(Error::TooLarge {
                cap: expected.cap,
                found: received,
            });
        }
        if let Some(size) = expected.size.filter(|&size| received > size) {
            return Err(Error::Size {
                expected: size,
                found: received,
            });
        }
        hasher.update(&chunk);
        writer.write_all(&chunk).await?;
        on_download(received, total);
    }
    writer.flush().await?;
    if let Some(size) = expected.size.filter(|&size| received != size) {
        return Err(Error::Size {
            expected: size,
            found: received,
        });
    }
    if hex_digest(&hasher.finalize()) != expected.digest {
        return Err(Error::Checksum);
    }
    Ok(())
}
