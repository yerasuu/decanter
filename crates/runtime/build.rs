//! Names the launch backend once for the crate: `cfg(proton)` where games
//! run under Proton through `umu-run`, `not(proton)` where they run as the
//! native Windows programs they are.

use std::env;

fn main() {
    println!("cargo::rerun-if-changed=build.rs");
    println!("cargo::rustc-check-cfg=cfg(proton)");
    // Linux is the Proton backend's home; macOS compiles it too only until it
    // has a backend of its own, and its capability table leaves Play off.
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        println!("cargo::rustc-cfg=proton");
    }
}
