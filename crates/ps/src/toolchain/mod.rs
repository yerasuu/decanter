//! Which toolchain builds and runs a port, picked without asking.
//!
//! The host's own tools when they cover the port, then the system's nix,
//! then decanter's own nix-portable, downloaded the first time it is
//! needed. The Vulkan drivers follow the same choice: a port built by the
//! host loads the host's Mesa, one built by nix only loads nix's, which is
//! why every nix environment here sets `VK_DRIVER_FILES` itself and why a
//! port is run inside the toolchain that built it.

mod portable;

use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use tokio::process::Command;

use crate::port::Port;

const PKGS_NIX: &str = include_str!("pkgs.nix");
const BBPORT_NIX: &str = include_str!("bbport.nix");
const ANYPS5_NIX: &str = include_str!("anyps5.nix");

/// What bbport's `build.sh` checks for before it gives up on the host,
/// plus Python for `run.sh`'s helpers.
const BBPORT_HOST_CHECK: &str = "command -v cc && command -v cmake && command -v ninja && command -v python3 \
     && command -v pkg-config && pkg-config --exists vulkan sdl3";

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
    #[error("no se pudo descargar nix-portable: {0}")]
    Download(#[from] reqwest::Error),
    #[error("la descarga de nix-portable no coincide con su suma fijada")]
    Checksum,
    #[error("{0}")]
    Io(#[from] io::Error),
}

impl Toolchain {
    /// May download nix-portable (about 65 MB), reporting bytes so far and
    /// the total when the server sends one.
    pub async fn resolve(port: Port, on_download: impl FnMut(u64, Option<u64>)) -> Result<Self, Error> {
        if host_covers(port).await {
            tracing::info!(?port, "host tools cover the port");
            return Ok(Self::Host);
        }
        if let Some(nix_shell) = system_nix_shell().await {
            tracing::info!(?port, nix_shell = %nix_shell.display(), "using the system's nix");
            return Ok(Self::SystemNix(nix_shell));
        }
        match portable::ensure(on_download).await {
            Ok(path) => {
                tracing::info!(?port, path = %path.display(), "using decanter's nix-portable");
                Ok(Self::Portable(path))
            }
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
        format!("{} {}", self.id(), crate::failures::commit_of(checkout).await)
    }

    /// For the UI's informative line; there is no choice to make.
    pub const fn label(&self) -> &'static str {
        match self {
            Self::Host => "herramientas del sistema",
            Self::SystemNix(_) => "nix del sistema",
            Self::Portable(_) => "nix-portable de decanter",
        }
    }

    /// `script` run by bash inside `port`'s environment, from `checkout`.
    /// Callers that run something elsewhere (a converted game) set their
    /// own `current_dir` after.
    pub async fn command(&self, port: Port, checkout: &Path, script: &str) -> io::Result<Command> {
        let mut command = match self {
            Self::Host => {
                let mut command = Command::new("bash");
                command.arg("-c").arg(script);
                command
            }
            Self::SystemNix(nix_shell) => {
                let env_file = match write_env_files(&crate::paths::toolchain().join("env")).await {
                    Ok(dir) => dir.join(env_file(port)),
                    Err(err) => return Err(err),
                };
                let mut command = Command::new(nix_shell);
                command.args(nix_shell_args(&env_file, checkout, script));
                command
            }
            Self::Portable(nix_portable) => {
                let env_file = match write_env_files(&crate::paths::toolchain().join("env")).await {
                    Ok(dir) => dir.join(env_file(port)),
                    Err(err) => return Err(err),
                };
                let mut command = Command::new(nix_portable);
                // Its store goes beside it rather than in `~/.nix-portable`.
                command
                    .env("NP_LOCATION", crate::paths::toolchain())
                    .arg("nix-shell")
                    .args(nix_shell_args(&env_file, checkout, script));
                command
            }
        };
        command.current_dir(checkout).stdin(Stdio::null());
        decanter_core::strip_launcher_env(command.as_std_mut());
        Ok(command)
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nix_shell_enters_the_port_environment_from_its_checkout() {
        let args = nix_shell_args(
            Path::new("/data/env/bbport.nix"),
            Path::new("/data/ports/bbport"),
            "bash build.sh",
        );
        assert_eq!(
            args,
            [
                "/data/env/bbport.nix",
                "--argstr",
                "checkout",
                "/data/ports/bbport",
                "--run",
                "bash build.sh"
            ]
            .map(OsString::from)
        );
    }

    #[tokio::test]
    async fn env_files_are_written_once_and_kept() {
        let dir = std::env::temp_dir().join(format!("decanter-ps-env-{}", std::process::id()));
        write_env_files(&dir).await.unwrap();
        let first = std::fs::metadata(dir.join("bbport.nix")).unwrap().modified().unwrap();
        write_env_files(&dir).await.unwrap();
        let second = std::fs::metadata(dir.join("bbport.nix")).unwrap().modified().unwrap();
        let pkgs = std::fs::read_to_string(dir.join("pkgs.nix")).unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!(first, second);
        assert_eq!(pkgs, PKGS_NIX);
    }
}
