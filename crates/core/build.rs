//! The commit this build comes from, for logs to name (`BUILD` in `lib.rs`);
//! `unknown` outside a git checkout, such as a source tarball.

#![expect(
    clippy::implicit_return,
    reason = "directly contradicts needless_return from clippy::all, also enabled"
)]

use std::process::Command;

/// `git`'s trimmed output, `None` when it fails or isn't there.
fn git(args: &[&str]) -> Option<String> {
    Command::new("git")
        .args(args)
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .map(|text| text.trim().to_owned())
}

fn main() {
    println!("cargo::rerun-if-changed=build.rs");
    // HEAD's reflog moves on every commit, checkout, reset and rebase.
    if let Some(dir) = git(&["rev-parse", "--absolute-git-dir"]) {
        println!("cargo::rerun-if-changed={dir}/HEAD");
        println!("cargo::rerun-if-changed={dir}/logs/HEAD");
    }
    let commit = git(&["rev-parse", "--short=12", "HEAD"]).unwrap_or_else(|| "unknown".to_owned());
    println!("cargo::rustc-env=DECANTER_COMMIT={commit}");
}
