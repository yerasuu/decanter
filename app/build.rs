//! rust-i18n's `i18n!` reads `locales/` at compile time without telling
//! cargo, so an edited translation would otherwise leave a stale build.

#![expect(
    clippy::print_stdout,
    reason = "cargo reads a build script's directives from its stdout"
)]

fn main() {
    println!("cargo::rerun-if-changed=locales");
}
