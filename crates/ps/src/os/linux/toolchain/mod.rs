//! psport's recipes for the nix toolchain ([`decanter_nix`]): each port's environment and the host
//! tools that cover its build, under the library's `psport/toolchain` folder.

use std::io;
use std::path::Path;

use decanter_nix::{Recipe, Toolchain};
use tokio::process::Command;

use crate::paths::Root;
use crate::port::Port;

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

pub const fn recipe(port: Port) -> Recipe {
    match port {
        Port::Bbport => Recipe {
            name: port.name(),
            env_file: "bbport.nix",
            env: BBPORT_NIX,
            host_check: BBPORT_HOST_CHECK,
        },
        Port::AnyPs5 => Recipe {
            name: port.name(),
            env_file: "anyps5.nix",
            env: ANYPS5_NIX,
            host_check: ANYPS5_HOST_CHECK,
        },
    }
}

/// `script` run by bash inside `port`'s environment, from its checkout under `root`.
pub async fn command(toolchain: &Toolchain, root: &Root, port: Port, script: &str) -> io::Result<Command> {
    toolchain
        .command(&root.toolchain(), &recipe(port), &port.checkout(root), script)
        .await
}

/// What a port's build marker holds: this toolchain and the checkout's
/// commit, so a checkout that moved (an update applied before the port
/// was prepared this session) is built again rather than run stale.
pub async fn build_stamp(toolchain: &Toolchain, checkout: &Path) -> String {
    format!("{} {}", toolchain.id(), crate::failures::full_commit_of(checkout).await)
}
