//! Which toolchain builds and runs a port, picked without asking.
//!
//! The host's own tools when they cover the port, then the system's nix,
//! then decanter's own nix-portable, downloaded the first time it is
//! needed. The Vulkan drivers follow the same choice: a port built by the
//! host loads the host's Mesa, one built by nix only loads nix's, which is
//! why every nix environment here sets `VK_DRIVER_FILES` itself and why a
//! port is run inside the toolchain that built it.
//!
//! The host check can't foresee every package a port's build requires, so
//! a host that fails to configure a port is set aside for nix from then on.

mod portable;

use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use tokio::process::Command;

use crate::paths::Root;
use crate::port::Port;

const PKGS_NIX: &str = include_str!("pkgs.nix");
const BBPORT_NIX: &str = include_str!("bbport.nix");
const ANYPS5_NIX: &str = include_str!("anyps5.nix");

/// What bbport's `build.sh` checks for before it gives up on the host,
/// plus Python for `run.sh`'s helpers.
const BBPORT_HOST_CHECK: &str = "command -v cc && command -v cmake && command -v ninja && command -v python3 \
     && command -v pkg-config && pkg-config --exists vulkan sdl3 freetype2";

/// AnyPS5 links its executables against a static glibc, which few hosts
/// ship: `-print-file-name` echoes the bare name back when it has none.
const ANYPS5_HOST_CHECK: &str = "command -v cc && command -v cmake && command -v ninja && command -v python3 \
     && command -v git && command -v pkg-config \
     && case $(cc -print-file-name=libc.a) in /*) true ;; *) false ;; esac";

/// Where a nix install puts `nix-shell` when it isn't on `PATH`: a session
/// started from Steam's game mode has a minimal one.
const NIX_SHELL_FALLBACKS: [&str; 2] = [
    "/nix/var/nix/profiles/default/bin/nix-shell",
    ".nix-profile/bin/nix-shell",
];

const fn env_file(port: Port) -> &'static str {
    match port {
        Port::Bbport => "bbport.nix",
        Port::AnyPs5 => "anyps5.nix",
    }
}

const fn host_check(port: Port) -> &'static str {
    match port {
        Port::Bbport => BBPORT_HOST_CHECK,
        Port::AnyPs5 => ANYPS5_HOST_CHECK,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Toolchain {
    Host,
    SystemNix(PathBuf),
    Portable(PathBuf),
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("could not download nix-portable: {0}")]
    Download(#[from] reqwest::Error),
    #[error("the nix-portable download does not match its pinned checksum")]
    Checksum,
    #[error("{0}")]
    Io(#[from] io::Error),
}

impl Toolchain {
    /// May download nix-portable (about 65 MB), reporting bytes so far and
    /// the total when the server sends one.
    pub async fn resolve(root: &Root, port: Port, on_download: impl FnMut(u64, Option<u64>)) -> Result<Self, Error> {
        if host_covers(port).await {
            if !tokio::fs::try_exists(host_refused(root, port)).await.unwrap_or(false) {
                tracing::info!(?port, "host tools cover the port");
                return Ok(Self::Host);
            }
            tracing::info!(?port, "host tools failed to configure the port before");
        }
        Self::nix(root, port, on_download).await
    }

    /// What [`Toolchain::resolve`] falls back to, never the host.
    pub async fn nix(root: &Root, port: Port, on_download: impl FnMut(u64, Option<u64>)) -> Result<Self, Error> {
        if let Some(nix_shell) = system_nix_shell().await {
            tracing::info!(?port, nix_shell = %nix_shell.display(), "using the system's nix");
            return Ok(Self::SystemNix(nix_shell));
        }
        match portable::ensure(root, on_download).await {
            Ok(path) => {
                tracing::info!(?port, path = %path.display(), "using decanter's nix-portable");
                Ok(Self::Portable(path))
            }
            Err(err) => Err(err),
        }
    }

    /// After the host's tools failed to configure `port`: later resolves
    /// skip the host for it until [`Toolchain::allow_host`].
    pub async fn refuse_host(root: &Root, port: Port) -> io::Result<()> {
        tokio::fs::create_dir_all(root.toolchain()).await?;
        tokio::fs::write(host_refused(root, port), b"").await
    }

    /// Once `port`'s checkout moved, since the new commit may configure with
    /// the host's tools after all. True when the host had been refused: the
    /// checkout's build caches are then nix's, which the caller must forget.
    pub async fn allow_host(root: &Root, port: Port) -> io::Result<bool> {
        match tokio::fs::remove_file(host_refused(root, port)).await {
            Ok(()) => Ok(true),
            Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(err) => Err(err),
        }
    }

    /// Stable across runs, to tell which toolchain a build came from.
    pub const fn id(&self) -> &'static str {
        match self {
            Self::Host => "host",
            Self::SystemNix(_) => "system-nix",
            Self::Portable(_) => "nix-portable",
        }
    }

    /// What a port's build marker holds: this toolchain and the checkout's
    /// commit, so a checkout that moved (an update applied before the port
    /// was prepared this session) is built again rather than run stale.
    pub async fn build_stamp(&self, checkout: &Path) -> String {
        format!("{} {}", self.id(), crate::failures::full_commit_of(checkout).await)
    }

    /// Where `store_path` of this toolchain's store is seen from outside it:
    /// nix-portable mounts its store, kept under the library, at `/nix/store`
    /// only for what it runs.
    pub fn store_path_on_host(&self, root: &Root, store_path: &Path) -> PathBuf {
        match (self, store_path.strip_prefix("/")) {
            (Self::Portable(_), Ok(relative)) => root.toolchain().join(".nix-portable").join(relative),
            _ => store_path.to_owned(),
        }
    }

    /// `script` run by bash inside `port`'s environment, from its checkout
    /// under `root`. Callers that run something elsewhere (a converted game)
    /// set their own `current_dir` after.
    pub async fn command(&self, root: &Root, port: Port, script: &str) -> io::Result<Command> {
        let checkout = port.checkout(root);
        let mut command = match self {
            Self::Host => {
                let mut command = Command::new("bash");
                command.arg("-c").arg(script);
                command
            }
            Self::SystemNix(nix_shell) => {
                let env_file = match write_env_files(&root.toolchain().join("env")).await {
                    Ok(dir) => dir.join(env_file(port)),
                    Err(err) => return Err(err),
                };
                let mut command = Command::new(nix_shell);
                command.args(nix_shell_args(&env_file, &checkout, script));
                command
            }
            Self::Portable(nix_portable) => {
                let env_file = match write_env_files(&root.toolchain().join("env")).await {
                    Ok(dir) => dir.join(env_file(port)),
                    Err(err) => return Err(err),
                };
                let mut command = Command::new(nix_portable);
                // Its store goes beside it rather than in `~/.nix-portable`.
                command
                    .env("NP_LOCATION", root.toolchain())
                    .arg("nix-shell")
                    .args(nix_shell_args(&env_file, &checkout, script));
                command
            }
        };
        command.current_dir(&checkout).stdin(Stdio::null());
        decanter_core::strip_launcher_env(command.as_std_mut());
        Ok(command)
    }

    /// Copies `store_path`'s closure from the binary cache at `cache` into
    /// this toolchain's store and roots it at `gc_root`. `None` for the host,
    /// which has no store: a prebuilt port only runs inside nix.
    pub fn fetch_command(&self, root: &Root, cache: &str, store_path: &str, gc_root: &Path) -> Option<Command> {
        let mut command = Command::new("bash");
        command
            .arg("-c")
            .arg(
                "set -e; cache=$1 path=$2 root=$3; shift 3; mkdir -p -- \"$(dirname -- \"$root\")\"; \
                 \"$@\" nix --extra-experimental-features nix-command copy --no-check-sigs --from \"$cache\" -- \"$path\"; \
                 \"$@\" nix-store --realise --add-root \"$root\" -- \"$path\" >/dev/null; \
                 echo \"prebuilt from cache: $path\"",
            )
            .arg("fetch")
            .arg(cache)
            .arg(store_path)
            .arg(gc_root);
        match self {
            Self::Host => return None,
            Self::SystemNix(nix_shell) => {
                // `nix` and `nix-store` sit beside `nix-shell`, which may have been found off `PATH`.
                let dir = nix_shell.parent().map(Path::to_owned).unwrap_or_default();
                let inherited = std::env::var_os("PATH").unwrap_or_default();
                if let Ok(search) = std::env::join_paths(std::iter::once(dir).chain(std::env::split_paths(&inherited)))
                {
                    command.env("PATH", search);
                }
            }
            Self::Portable(nix_portable) => {
                command.env("NP_LOCATION", root.toolchain()).arg(nix_portable);
            }
        }
        command.stdin(Stdio::null());
        decanter_core::strip_launcher_env(command.as_std_mut());
        Some(command)
    }
}

fn host_refused(root: &Root, port: Port) -> PathBuf {
    root.toolchain()
        .join(format!("host-refused-{}", port.name().to_lowercase()))
}

fn nix_shell_args(env_file: &Path, checkout: &Path, script: &str) -> [OsString; 6] {
    [
        env_file.into(),
        "--argstr".into(),
        "checkout".into(),
        checkout.into(),
        "--run".into(),
        script.into(),
    ]
}

/// The environments, rewritten only when this build of decanter carries
/// different ones, so nix-shell's evaluation cache stays warm.
async fn write_env_files(dir: &Path) -> io::Result<PathBuf> {
    if let Err(err) = tokio::fs::create_dir_all(dir).await {
        return Err(err);
    }
    for (name, text) in [
        ("pkgs.nix", PKGS_NIX),
        ("bbport.nix", BBPORT_NIX),
        ("anyps5.nix", ANYPS5_NIX),
    ] {
        let path = dir.join(name);
        let current = tokio::fs::read_to_string(&path).await.ok();
        if current.as_deref() != Some(text)
            && let Err(err) = tokio::fs::write(&path, text).await
        {
            return Err(err);
        }
    }
    Ok(dir.to_owned())
}

async fn host_covers(port: Port) -> bool {
    let mut check = Command::new("bash");
    check
        .arg("-c")
        .arg(host_check(port))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    // The AppImage's own library path would make host tools load its libs.
    decanter_core::strip_launcher_env(check.as_std_mut());
    check.status().await.is_ok_and(|status| status.success())
}

async fn system_nix_shell() -> Option<PathBuf> {
    let path = std::env::var_os("PATH").unwrap_or_default();
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
    let on_path = std::env::split_paths(&path).map(|dir| dir.join("nix-shell"));
    let fallbacks = NIX_SHELL_FALLBACKS.iter().map(|candidate| home.join(candidate));
    for candidate in on_path.chain(fallbacks) {
        if tokio::fs::metadata(&candidate).await.is_ok_and(|meta| meta.is_file()) {
            return Some(candidate);
        }
    }
    None
}
