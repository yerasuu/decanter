//! Builds `crates/hook` into `Galaxy64.dll` and hands the result
//! to `steam_export`/`install` (well, not yet -- see that module) as
//! `env!("GALAXY64_DLL_PATH")`, embedded via `include_bytes!`.
//!
//! `decanter-hook` is a separate, workspace-excluded crate (it cross-
//! compiles a Windows cdylib, which the main workspace's lints and
//! `Cargo.lock` don't fit) rather than a dependency here, so this shells
//! out to a nested `cargo build` instead of a normal Rust build-dependency.
//! Cross-compiling from Linux is gated on the dev shell providing a real linker
//! (see `shell.nix`'s `CARGO_TARGET_X86_64_PC_WINDOWS_GNU_LINKER`) --
//! failing the build outright when it's missing beats silently shipping
//! decanter without the achievement hook and finding out at runtime.

#![expect(
    clippy::panic,
    clippy::expect_used,
    reason = "a panic is how a build script fails the build; it never runs in the shipped binary"
)]

use std::env;
use std::path::PathBuf;
use std::process::Command;

fn main() {
    println!("cargo::rustc-check-cfg=cfg(galaxy_hook)");
    // Games are x86_64 Windows programs, under Proton on Linux and natively
    // on Windows; `src/hook.rs` leaves the hook out of every other target.
    // A Windows build reuses its own target so a native MSVC host needs no
    // second toolchain.
    let hook_target = match (
        env::var("CARGO_CFG_TARGET_OS").as_deref(),
        env::var("CARGO_CFG_TARGET_ARCH").as_deref(),
    ) {
        (Ok("linux"), _) => "x86_64-pc-windows-gnu".to_owned(),
        (Ok("windows"), Ok("x86_64")) => env::var("TARGET").expect("cargo always sets TARGET for a build script"),
        _ => return,
    };
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let hook_dir = manifest_dir.join("../hook");
    let hook_manifest = hook_dir.join("Cargo.toml");

    // Only re-run this (genuinely slow -- a full cross-compile) when the
    // hook crate's own inputs actually changed, not on every rebuild of
    // this crate.
    println!("cargo::rerun-if-changed={}", hook_dir.join("src").display());
    println!("cargo::rerun-if-changed={}", hook_manifest.display());
    println!("cargo::rerun-if-env-changed=CARGO_TARGET_X86_64_PC_WINDOWS_GNU_LINKER");

    let out_dir = PathBuf::from(env::var("OUT_DIR").expect("cargo always sets OUT_DIR for a build script"));
    let hook_target_dir = out_dir.join("hook-target");

    let status = Command::new(env!("CARGO"))
        .arg("build")
        .arg("--release")
        .arg("--target")
        .arg(&hook_target)
        .arg("--manifest-path")
        .arg(&hook_manifest)
        .arg("--target-dir")
        .arg(&hook_target_dir)
        // Cargo hands a build script `CARGO_ENCODED_RUSTFLAGS` for *this*
        // build (here, empty -- decanter-gog's own host target has none
        // set), and that variable outranks `RUSTFLAGS` and every
        // `CARGO_TARGET_<triple>_RUSTFLAGS` in cargo's own precedence
        // rules. Left in place, the nested cargo above inherits and
        // obeys that empty value instead of ever consulting
        // `CARGO_TARGET_X86_64_PC_WINDOWS_GNU_RUSTFLAGS` (set in
        // shell.nix) for its own, unrelated target -- confirmed the hard
        // way: the link failed for a missing `-L`, which was verified
        // present in this same process's own environment.
        .env_remove("CARGO_ENCODED_RUSTFLAGS")
        .env_remove("RUSTFLAGS")
        .status()
        .unwrap_or_else(|err| {
            panic!(
                "could not run `cargo build` for crates/hook ({err}) -- \
                 the achievement hook needs the {hook_target} target; see shell.nix"
            )
        });
    if !status.success() {
        panic!(
            "building crates/hook into Galaxy64.dll for {hook_target} failed ({status}) -- \
             cross-compiling needs CARGO_TARGET_X86_64_PC_WINDOWS_GNU_LINKER; see shell.nix"
        );
    }

    let dll_path = hook_target_dir.join(&hook_target).join("release").join("Galaxy64.dll");
    assert!(
        dll_path.is_file(),
        "crates/hook built successfully but {} is missing",
        dll_path.display()
    );

    // `include_bytes!` needs a path fixed at compile time, hence the env
    // var rather than returning this -- `OUT_DIR` itself isn't stable
    // across a `cargo clean`, so the path is re-derived by decanter-gog's
    // own build every time rather than cached anywhere longer-lived.
    println!("cargo::rustc-env=GALAXY64_DLL_PATH={}", dll_path.display());
    println!("cargo::rustc-cfg=galaxy_hook");
}
