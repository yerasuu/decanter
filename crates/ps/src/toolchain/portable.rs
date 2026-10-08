//! decanter's own nix-portable, for hosts with neither the build tools
//! nor nix. It runs nix without root, from a store under decanter's data
//! directory, so nothing outside that directory changes.

use std::path::PathBuf;

use sha2::{Digest as _, Sha256};
use tokio::io::AsyncWriteExt as _;

use super::Error;

/// Pinned and bumped deliberately, like `umu-run`: a new nix-portable
/// changing how the ports build is worth testing first.
const VERSION: &str = "v012";

/// Of the release asset, taken when [`VERSION`] was pinned; the release
/// publishes no checksum of its own.
const SHA256: &str = "b409c55904c909ac3aeda3fb1253319f86a89ddd1ba31a5dec33d4a06414c72a";

fn path() -> PathBuf {
    crate::paths::toolchain().join("nix-portable")
}

/// Only a download whose checksum matched is ever renamed into place, so
/// a file at [`path`] is trusted as is.
pub(super) async fn ensure(mut on_download: impl FnMut(u64, Option<u64>)) -> Result<PathBuf, Error> {
    let path = path();
    if tokio::fs::metadata(&path).await.is_ok_and(|meta| meta.is_file()) {
        return Ok(path);
    }
    let url = format!("https://github.com/DavHau/nix-portable/releases/download/{VERSION}/nix-portable-x86_64");
    tracing::info!(url, "downloading nix-portable");
    let mut response = match reqwest::get(&url).await.and_then(reqwest::Response::error_for_status) {
        Ok(response) => response,
        Err(err) => return Err(err.into()),
    };
    let total = response.content_length();
    // Per process, so two decanter processes downloading at once don't
    // write into each other's file.
    let partial = path.with_extension(format!("part-{}", std::process::id()));
    if let Some(parent) = path.parent()
        && let Err(err) = tokio::fs::create_dir_all(parent).await
    {
        return Err(err.into());
    }
    let mut file = match tokio::fs::File::create(&partial).await {
        Ok(file) => file,
        Err(err) => return Err(err.into()),
    };
    let mut hasher = Sha256::new();
    let mut received = 0;
    loop {
        let chunk = match response.chunk().await {
            Ok(Some(chunk)) => chunk,
            Ok(None) => break,
            Err(err) => return Err(err.into()),
        };
        hasher.update(&chunk);
        if let Err(err) = file.write_all(&chunk).await {
            return Err(err.into());
        }
        received += chunk.len() as u64;
        on_download(received, total);
    }
    if let Err(err) = file.flush().await {
        return Err(err.into());
    }
    drop(file);
    if format!("{:x}", hasher.finalize()) != SHA256 {
        let _ = tokio::fs::remove_file(&partial).await;
        tracing::error!(url, "nix-portable download does not match its pinned checksum");
        return Err(Error::Checksum);
    }
    {
        use std::os::unix::fs::PermissionsExt as _;
        if let Err(err) = tokio::fs::set_permissions(&partial, std::fs::Permissions::from_mode(0o755)).await {
            return Err(err.into());
        }
    }
    if let Err(err) = tokio::fs::rename(&partial, &path).await {
        return Err(err.into());
    }
    tracing::info!(path = %path.display(), "nix-portable installed");
    Ok(path)
}
