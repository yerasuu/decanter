//! Khronos' Vulkan validation layer, for the port's validation setting. No environment the port
//! runs in carries it, so the first launch that asks for it realises it from the pinned nixpkgs
//! (decision #474), rooted under the library's toolchain folder and reused while the pin holds.

use std::ffi::OsStr;
use std::io;
use std::path::{Path, PathBuf};

use sha2::{Digest as _, Sha256};
use tokio::process::Command;

use super::{PKGS_NIX, Toolchain, VALIDATION_NIX, system_nix_shell, write_env_files};
use crate::paths::Root;

pub(super) const ENV_FILE: &str = "vulkan-validation.nix";

/// Why the port gets no layer, so it starts without validating.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum Unavailable {
    /// nix-portable's store is visible only to what nix-portable runs, so a port run on the host
    /// (its own build or a bundle) needs the system's nix.
    #[error("the port runs on the host and the host has no nix")]
    NoNix,
    #[error("could not write the layer's nix environment or record")]
    Setup,
    #[error("nix could not fetch or build vulkan-validation-layers")]
    Build,
}

impl Unavailable {
    const fn code(self) -> &'static str {
        match self {
            Self::NoNix => "no-nix",
            Self::Setup => "setup",
            Self::Build => "build",
        }
    }

    fn from_code(code: &str) -> Option<Self> {
        [Self::NoNix, Self::Setup, Self::Build]
            .into_iter()
            .find(|why| why.code() == code)
    }
}

fn dir(root: &Root) -> PathBuf {
    root.toolchain().join("vulkan-validation")
}

/// nix's out-link, which also keeps the layer from its garbage collector.
fn link(root: &Root, realiser: &Toolchain) -> PathBuf {
    dir(root).join(realiser.id())
}

fn stamp(link: &Path) -> PathBuf {
    link.with_extension("pin")
}

fn unavailable_path(root: &Root) -> PathBuf {
    dir(root).join("unavailable")
}

/// Changes with the nixpkgs pin, so a layer realised from an older one is realised again.
fn pin() -> String {
    let mut hasher = Sha256::new();
    hasher.update(PKGS_NIX);
    hasher.update(VALIDATION_NIX);
    crate::download::hex_digest(&hasher.finalize())
}

/// The nix that realises the layer for a port run by `toolchain`: the same one, or the system's
/// for a port run on the host, whose `/nix/store` is the host's own.
pub async fn realiser(toolchain: &Toolchain) -> Result<Toolchain, Unavailable> {
    match toolchain {
        Toolchain::SystemNix(_) | Toolchain::Portable(_) => Ok(toolchain.clone()),
        Toolchain::Host | Toolchain::Bundle => system_nix_shell()
            .await
            .map(Toolchain::SystemNix)
            .ok_or(Unavailable::NoNix),
    }
}

/// The folder for `VK_ADD_LAYER_PATH` as `realiser`'s environment sees it, once a launch
/// realised it from the current pin.
pub async fn cached(root: &Root, realiser: &Toolchain) -> Option<PathBuf> {
    let link = link(root, realiser);
    let stamped = tokio::fs::read_to_string(stamp(&link)).await.ok()?;
    if stamped.trim() != pin() {
        return None;
    }
    let layers = tokio::fs::read_link(&link)
        .await
        .ok()?
        .join("share/vulkan/explicit_layer.d");
    tokio::fs::metadata(realiser.store_path_on_host(root, &layers))
        .await
        .is_ok_and(|meta| meta.is_dir())
        .then_some(layers)
}

/// Realises the layer into `realiser`'s store where [`cached`] looks, with nix's progress as its
/// output; [`record`] once it succeeded.
pub async fn realise_command(root: &Root, realiser: &Toolchain) -> io::Result<Command> {
    let env_file = write_env_files(&root.toolchain().join("env")).await?.join(ENV_FILE);
    let link = link(root, realiser);
    realiser
        .nix_script(
            root,
            "set -e; file=$1 link=$2; shift 2; mkdir -p -- \"$(dirname -- \"$link\")\"; \
             \"$@\" nix-build --out-link \"$link\" -- \"$file\"",
            &[OsStr::new("vulkan-validation"), env_file.as_os_str(), link.as_os_str()],
        )
        .ok_or_else(|| io::Error::new(io::ErrorKind::Unsupported, "no nix to realise the layer with"))
}

/// Stamps the pin [`realise_command`] realised the layer from, after it succeeded.
pub async fn record(root: &Root, realiser: &Toolchain) -> Result<PathBuf, Unavailable> {
    let link = link(root, realiser);
    if let Err(err) = tokio::fs::write(stamp(&link), pin()).await {
        tracing::warn!(path = %link.display(), %err, "could not stamp the realised validation layer");
        return Err(Unavailable::Setup);
    }
    cached(root, realiser).await.ok_or(Unavailable::Build)
}

/// Kept for the setting's note until a launch gets the layer.
pub async fn record_unavailable(root: &Root, why: Unavailable) -> io::Result<()> {
    tokio::fs::create_dir_all(dir(root)).await?;
    tokio::fs::write(unavailable_path(root), why.code()).await
}

pub async fn clear_unavailable(root: &Root) -> io::Result<()> {
    match tokio::fs::remove_file(unavailable_path(root)).await {
        Err(err) if err.kind() != io::ErrorKind::NotFound => Err(err),
        _ => Ok(()),
    }
}

/// Why the last launch that asked for the layer got none. File I/O: off the UI thread.
pub fn last_unavailable_blocking(root: &Root) -> Option<Unavailable> {
    std::fs::read_to_string(unavailable_path(root))
        .ok()
        .and_then(|code| Unavailable::from_code(code.trim()))
}
