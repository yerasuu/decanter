//! rust-i18n's `i18n!` reads `locales/` at compile time without telling
//! cargo, so an edited translation would otherwise leave a stale build.
//!
//! The minisign key decanter trusts for its own updates is read here from
//! `keys/release.pub`, so a key that doesn't parse fails the build instead
//! of shipping a decanter that can never verify a release. Empty, the build
//! goes on without one and the updater says so.

#![expect(
    clippy::print_stdout,
    reason = "cargo reads a build script's directives from its stdout"
)]

use std::path::PathBuf;

fn main() -> Result<(), String> {
    println!("cargo::rerun-if-changed=locales");
    println!("cargo::rerun-if-changed=../keys/release.pub");
    embed_windows_resources()?;

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

/// Gated on the target, not the host, so the Linux-hosted cross build to
/// windows-gnu (which needs `windres` on PATH) embeds it too.
fn embed_windows_resources() -> Result<(), String> {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return Ok(());
    }
    println!("cargo::rerun-if-changed=assets/decanter.rc");
    println!("cargo::rerun-if-changed=assets/favicon.ico");
    let version = |key: &str| std::env::var(key).map_err(|err| format!("cargo didn't set {key}: {err}"));
    let major = version("CARGO_PKG_VERSION_MAJOR")?;
    let minor = version("CARGO_PKG_VERSION_MINOR")?;
    let patch = version("CARGO_PKG_VERSION_PATCH")?;
    let macros = [
        format!("DECANTER_VERSION_COMMA={major},{minor},{patch},0"),
        format!("DECANTER_VERSION={major}.{minor}.{patch}"),
    ];
    embed_resource::compile("assets/decanter.rc", macros)
        .manifest_required()
        .map_err(|err| format!("cannot embed the Windows resources: {err}"))
}
