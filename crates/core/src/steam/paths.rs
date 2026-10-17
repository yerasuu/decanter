//! Locating the real Steam installation on this machine — its root
//! directory, which local user is logged in, and where that user's
//! `shortcuts.vdf` and grid artwork live.
//!
//! Entirely separate from `crates/runtime::steam_compat_dir`, which
//! resolves a sibling path (`Steam/compatibilitytools.d`, for Proton
//! builds) — this module resolves `Steam/userdata/<id>/...` instead, but
//! mirrors that function's `XDG_DATA_HOME`-then-`HOME` fallback rather
//! than inventing a new convention.

#![expect(
    clippy::std_instead_of_alloc,
    reason = "desktop app, no no_std target exists to justify alloc:: here"
)]
#![expect(
    clippy::std_instead_of_core,
    reason = "desktop app, no no_std target exists to justify core:: here"
)]
#![expect(
    clippy::implicit_return,
    reason = "directly contradicts needless_return from clippy::all, also enabled"
)]

use std::fs;
use std::path::{Path, PathBuf};

use crate::{platform, vdf};

/// A `SteamID64`'s offset from the account id used for `userdata/<id>` —
/// every Steam client folder is named this way, regardless of platform.
const STEAM_ID64_ACCOUNT_OFFSET: u64 = 76_561_197_960_265_728;

/// The first candidate Steam installation that actually has a `userdata`
/// directory, in [`crate::platform::steam_root_candidates`]' order.
#[must_use]
#[inline]
pub fn steam_root() -> Option<PathBuf> {
    platform::steam_root_candidates().find(|candidate| candidate.join("userdata").is_dir())
}

/// The account id (as used in `userdata/<id>`) of whichever local Steam
/// user last used this client, read from `config/loginusers.vdf`.
///
/// `None` if the file is missing, unreadable, or empty — a Steam install
/// nobody has ever logged into.
#[must_use]
#[inline]
pub fn local_user_id(steam_root: &Path) -> Option<String> {
    let path = steam_root.join("config").join("loginusers.vdf");
    let content = match fs::read_to_string(&path) {
        Ok(content) => content,
        Err(err) => {
            tracing::debug!(path = %path.display(), %err, "no loginusers.vdf to read");
            return None;
        }
    };
    let (_root_key, root) = match vdf::from_str(&content) {
        Ok(parsed) => parsed,
        Err(err) => {
            tracing::warn!(path = %path.display(), %err, "failed to parse loginusers.vdf");
            return None;
        }
    };
    let Some(users) = root.as_block() else {
        return None;
    };

    let most_recent = users
        .iter()
        .filter(|(_, entry)| entry.get("mostrecent") == Some("1"))
        .max_by_key(|(_, entry)| entry.get_u64("timestamp").unwrap_or(0));
    let Some((steam_id64, _)) = most_recent.or_else(|| {
        users
            .iter()
            .max_by_key(|(_, entry)| entry.get_u64("timestamp").unwrap_or(0))
    }) else {
        return None;
    };

    let Ok(steam_id64) = steam_id64.parse::<u64>() else {
        return None;
    };
    let Some(account_id) = steam_id64.checked_sub(STEAM_ID64_ACCOUNT_OFFSET) else {
        return None;
    };
    Some(account_id.to_string())
}

#[must_use]
#[inline]
pub fn shortcuts_vdf_path(steam_root: &Path, user_id: &str) -> PathBuf {
    steam_root
        .join("userdata")
        .join(user_id)
        .join("config")
        .join("shortcuts.vdf")
}

#[must_use]
#[inline]
pub fn grid_dir(steam_root: &Path, user_id: &str) -> PathBuf {
    steam_root.join("userdata").join(user_id).join("config").join("grid")
}
