//! rust-i18n's `i18n!` reads `locales/` at compile time without telling
//! cargo, so an edited translation would otherwise leave a stale build.
//!
//! The minisign key decanter trusts for its own updates is read here from
//! `keys/release.pub`, so a key that doesn't parse fails the build instead
//! of shipping a decanter that can never verify a release. Empty, the build
//! goes on without one and the updater says so.
//!
//! `cfg(proton)` names the launch backend once for the crate, as in
//! `decanter-runtime`'s build script: Proton through `umu-run` on Linux, or
//! native programs (Windows, macOS) where it's off.

#![expect(
    clippy::print_stdout,
    reason = "cargo reads a build script's directives from its stdout"
)]

use std::path::PathBuf;

fn main() -> Result<(), String> {
    println!("cargo::rerun-if-changed=locales");
    println!("cargo::rerun-if-changed=../keys/release.pub");
    println!("cargo::rustc-check-cfg=cfg(proton)");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("linux") {
        println!("cargo::rustc-cfg=proton");
    }

    let manifest = std::env::var_os("CARGO_MANIFEST_DIR").ok_or("cargo didn't set CARGO_MANIFEST_DIR")?;
    let out_dir = std::env::var_os("OUT_DIR").ok_or("cargo didn't set OUT_DIR")?;
    let source = PathBuf::from(manifest).join("../keys/release.pub");
    let text = match std::fs::read_to_string(&source) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(err) => return Err(format!("cannot read {}: {err}", source.display())),
    };
    let key = text.trim();
    if !key.is_empty() {
        minisign_verify::PublicKey::decode(key)
            .map_err(|err| format!("{} is not a minisign public key: {err}", source.display()))?;
    }
    std::fs::write(PathBuf::from(out_dir).join("release.pub"), key)
        .map_err(|err| format!("cannot write the embedded release key: {err}"))
}
