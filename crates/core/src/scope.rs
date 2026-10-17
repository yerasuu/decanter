//! Transient systemd scopes for the games decanter starts.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::capabilities::Feature;
use crate::platform;

/// What names a game's scope, desktop entry and icon.
///
/// `id` with anything but `[A-Za-z0-9_]` turned into `_`, since it may be a
/// uuid or come from a dump.
#[must_use]
#[inline]
pub fn game_key(id: &str) -> String {
    id.chars()
        .map(|ch| if ch.is_ascii_alphanumeric() { ch } else { '_' })
        .collect()
}

/// The XDG application id a game's window and scope carry, from its
/// [`game_key`].
#[must_use]
#[inline]
pub fn application_id(key: &str) -> String {
    format!("decanter.game.{key}")
}

/// `systemd-run` set up to start what follows in a transient scope of its
/// own, or `None` without a systemd user manager to ask.
///
/// Started as decanter's child, a game would sit in decanter's own scope
/// (`app-decanter@….service` when it was opened from the menu) and be
/// taken for part of decanter. Its own scope is named the way the XDG
/// convention names an app launched by another
/// (`app-decanter-<application id>-<random>.scope`) and described with the
/// game's title, which is what systemd and the desktop show for it.
/// `--scope` runs the command in this very process, so the child handle
/// still waits on the command itself, and its pid and process group stay
/// the same.
#[must_use]
#[inline]
pub fn own_scope(key: &str, description: &str) -> Option<Command> {
    if !platform::support(Feature::Scopes).is_offered() {
        return None;
    }
    let systemd_run = systemd_run()?;
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_nanos());
    let mut command = Command::new(systemd_run);
    command
        .args(["--user", "--scope", "--quiet", "--collect", "--unit"])
        .arg(format!(
            "app-decanter-{}-{:x}{nonce:x}.scope",
            application_id(&game_key(key)),
            std::process::id()
        ))
        .arg("--description")
        .arg(description)
        .arg("--");
    Some(command)
}

/// `systemd-run`, when a systemd user manager is there for it to ask.
#[must_use]
#[inline]
pub fn systemd_run() -> Option<PathBuf> {
    let runtime_dir = std::env::var_os("XDG_RUNTIME_DIR")?;
    if !Path::new(&runtime_dir).join("systemd/private").exists() {
        return None;
    }
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join("systemd-run"))
        .find(|candidate| candidate.is_file())
}
