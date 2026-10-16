//! Cross-compiles `crates/hook` into `Galaxy64.dll` and hands the result
//! to `steam_export`/`install` (well, not yet -- see that module) as
//! `env!("GALAXY64_DLL_PATH")`, embedded via `include_bytes!`.
//!
//! `decanter-hook` is a separate, workspace-excluded crate (it cross-
//! compiles a Windows cdylib, which the main workspace's lints and
//! `Cargo.lock` don't fit) rather than a dependency here, so this shells
//! out to a nested `cargo build` instead of a normal Rust build-dependency.
//! Gated on the project's own dev shell providing a real cross linker
//! (see `shell.nix`'s `CARGO_TARGET_X86_64_PC_WINDOWS_GNU_LINKER`) --
//! failing the build outright when it's missing beats silently shipping
//! decanter without the achievement hook and finding out at runtime.

#![expect(
    clippy::panic,
    clippy::expect_used,
    reason = "a panic is how a build script fails the build; it never runs in the shipped binary"
)]

use std::path::PathBuf;
use std::process::Command;

fn main() {
    // The hook only ships where games run under Proton; `src/hook.rs`
    // leaves it out of other targets rather than cross-compiling for them.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("linux") {
        return;
    }
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let hook_dir = manifest_dir.join("../hook");
    let hook_manifest = hook_dir.join("Cargo.toml");

    // Only re-run this (genuinely slow -- a full cross-compile) when the
    // hook crate's own inputs actually changed, not on every rebuild of
    // this crate.
    println!("cargo::rerun-if-changed={}", hook_dir.join("src").display());
    println!("cargo::rerun-if-changed={}", hook_manifest.display());
    println!("cargo::rerun-if-env-changed=CARGO_TARGET_X86_64_PC_WINDOWS_GNU_LINKER");

    let out_dir = PathBuf::from(std::env::var("OUT_DIR").expect("cargo always sets OUT_DIR for a build script"));
    let hook_target_dir = out_dir.join("hook-target");

    let status = Command::new(env!("CARGO"))
        .arg("build")
        .arg("--release")
        .arg("--target")
        .arg("x86_64-pc-windows-gnu")
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
                 the achievement hook needs a Windows cross target; see shell.nix"
            )
        });
    if !status.success() {
        panic!(
            "cross-compiling crates/hook into Galaxy64.dll failed ({status}) -- \
             is CARGO_TARGET_X86_64_PC_WINDOWS_GNU_LINKER set? see shell.nix"
        );
    }

    let dll_path: PathBuf = hook_target_dir.join("x86_64-pc-windows-gnu/release/Galaxy64.dll");
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
}
