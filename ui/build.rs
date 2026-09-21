//! Captures a few of nix-shell's own env vars at build time, when this
//! crate is in fact built under it -- `main.rs`'s own `relaunch_with_nix_env`
//! restores them at startup outside that shell, since WebKitGTK's own
//! `WebProcess` helper (an absolute path baked into `libwebkit2gtk` itself,
//! not something `dx bundle` relocates) is still Nix's own build no matter
//! how the final binary gets launched, and so are its own runtime
//! dependencies -- mesa's GL/EGL drivers, and glib-networking's TLS
//! backend + CA bundle, which every `https://` request the webview itself
//! makes (cover art, any other remote `<img>`) needs.
fn main() {
    for var in ["LD_LIBRARY_PATH", "GIO_EXTRA_MODULES", "SSL_CERT_FILE", "IN_NIX_SHELL"] {
        println!("cargo::rerun-if-env-changed={var}");
    }

    // Gated on `IN_NIX_SHELL` (nix-shell's own marker), not just "is the
    // var non-empty" -- a plain non-Nix build (see the README's "Without
    // Nix" section) might have its own unrelated values for these on the
    // build machine, which have no business being baked into the binary
    // as a fallback for every machine it later runs on.
    let built_under_nix_shell = std::env::var_os("IN_NIX_SHELL").is_some();
    for (var, out_key) in [
        ("LD_LIBRARY_PATH", "DECANTER_NIX_LD_LIBRARY_PATH"),
        ("GIO_EXTRA_MODULES", "DECANTER_NIX_GIO_EXTRA_MODULES"),
        ("SSL_CERT_FILE", "DECANTER_NIX_SSL_CERT_FILE"),
    ] {
        let value = if built_under_nix_shell { std::env::var(var).unwrap_or_default() } else { String::new() };
        println!("cargo::rustc-env={out_key}={value}");
    }
}
