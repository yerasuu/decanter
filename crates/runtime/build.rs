//! Names the launch backend once for the crate: `cfg(proton)` where games
//! run under Proton through `umu-run` (Linux), `not(proton)` where they run
//! as the native programs they are (Windows, macOS).

use std::env;

fn main() {
    println!("cargo::rerun-if-changed=build.rs");
    println!("cargo::rustc-check-cfg=cfg(proton)");
    if env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("linux") {
        println!("cargo::rustc-cfg=proton");
    }
}
