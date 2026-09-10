//! One-time adoption of the profiles achv used to read out of
//! bottles-next.
//!
//! Profile ids are what every cache row is keyed by — the library
//! listing, achievements, playtime, collections. Without this, dropping
//! the bottles-next dependency would leave all of it addressed by a
//! profile that no longer exists, i.e. an app that looks empty.
//!
//! Ids are carried over unchanged, so nothing else has to be rewritten.
//!
//! **Installs are deliberately not adopted.** bottles-next kept a game's
//! files inside its bottle's `drive_c`; achv keeps them in a shared
//! `common/` outside any prefix ([`crate::library`]). There is no way to
//! record one as the other without moving gigabytes, and a half-adopted
//! install pointing at files laid out the old way is worse for the user
//! than an honest reinstall.
//!
//! Runs at most once, and never overwrites anything achv already has.

// Only fires on some compilation targets (lib vs tests), so `expect` can't
// be used here without an "unfulfilled expectation" warning on the other.
#![allow(
    clippy::self_named_module_files,
    reason = "convention here is `<name>.rs` plus a sibling `<name>/tests.rs`, see CLAUDE.md"
)]
#![expect(
    clippy::implicit_return,
    reason = "directly contradicts `needless_return` from `clippy::all`, also enabled"
)]
#![expect(
    clippy::large_stack_frames,
    reason = "fires on every fn with a couple of locals and on serde-derived Deserialize impls \
              at the default 512-byte threshold, not actionable here"
)]
#![expect(
    clippy::single_call_fn,
    reason = "adopt_from and read_toml are split out on purpose so tests can exercise them \
              without going through the settings check in adopt_bottles_next_state"
)]
#![expect(
    clippy::pub_with_shorthand,
    reason = "wants `pub(in crate)`; this crate uses the `pub(crate)` shorthand everywhere else"
)]

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use serde::de::DeserializeOwned;

use crate::db::{Db, Result};

/// Marks the adoption as done, so a profile the user deletes afterwards
/// doesn't get handed back on the next start.
const DONE_SETTING: &str = "adopted_from_bottles_next";

#[derive(Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct Adopted {
    pub profiles: usize,
}

impl Adopted {
    #[inline]
    #[must_use]
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

/// The shape of bottles-next's `profiles.toml`, trimmed to what this reads.
#[derive(Debug, Deserialize)]
struct ProfilesFile {
    /// The profile bottles-next had selected, if any.
    active_profile_id: Option<String>,
    /// Every profile bottles-next knew about.
    #[serde(default)]
    profiles: Vec<ProfileEntry>,
}

/// One profile entry as bottles-next wrote it.
#[derive(Debug, Deserialize)]
struct ProfileEntry {
    /// Stable id, carried over unchanged so cache rows keyed by it stay valid.
    id: String,
    /// Display name shown in the profile picker.
    name: String,
}

/// Where bottles-next keeps the file this reads. Split out so the tests
/// can point at a fixture instead of the real home directory.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct BottlesNextPaths {
    pub profiles: PathBuf,
}

impl BottlesNextPaths {
    /// `None` on a platform with no config directory to look in.
    #[inline]
    #[must_use]
    pub fn discover() -> Option<Self> {
        let config_dir = match dirs::config_dir() {
            Some(dir) => dir,
            None => return None,
        };
        Some(Self {
            profiles: config_dir.join("bottles-next").join("profiles.toml"),
        })
    }
}

/// Imports whatever bottles-next left behind, once.
///
/// Best-effort: a file that is missing, unreadable or in a shape this no
/// longer recognizes is skipped rather than failing, because the
/// alternative is an app that won't start over data the user may not even
/// have.
// `missing_errors_doc` wants a `# Errors` section too, but the only honest
// content ("a db write failed") repeats the function's own `Result` return
// type, which is exactly the restates-the-signature comment this
// codebase's own humanization scanner rejects. Left without one.
#[inline]
#[expect(
    clippy::module_name_repetitions,
    reason = "public API, ui/src/core.rs calls it by this name"
)]
pub fn adopt_bottles_next_state(db: &Db) -> Result<Adopted> {
    let done = match db.setting(DONE_SETTING) {
        Ok(setting) => setting,
        Err(err) => return Err(err),
    };
    if done.is_some() {
        return Ok(Adopted::default());
    }
    let Some(paths) = BottlesNextPaths::discover() else {
        return Ok(Adopted::default());
    };
    let adopted = match adopt_from(db, &paths) {
        Ok(adopted) => adopted,
        Err(err) => return Err(err),
    };
    match db.set_setting(DONE_SETTING, "1") {
        Ok(()) => {}
        Err(err) => return Err(err),
    }
    Ok(adopted)
}

/// Reads `paths.profiles` and upserts whatever it finds into `db`.
pub(crate) fn adopt_from(db: &Db, paths: &BottlesNextPaths) -> Result<Adopted> {
    let mut adopted = Adopted::default();

    let Some(file) = read_toml::<ProfilesFile>(&paths.profiles) else {
        return Ok(adopted);
    };

    let known_profiles = match db.list_profiles() {
        Ok(profiles) => profiles,
        Err(err) => return Err(err),
    };
    let known: HashSet<String> = known_profiles.into_iter().map(|profile| profile.id).collect();
    for (position, profile) in file.profiles.iter().enumerate() {
        let rank = match i64::try_from(position) {
            Ok(rank) => rank,
            // A profile list long enough to overflow i64 isn't a real scenario.
            Err(err) => panic!("profile index overflow: {err}"),
        };
        // Upserted either way so a rename in bottles-next still lands, but
        // only counted as adopted the first time — the count is what tells
        // the caller whether anything actually came over.
        if let Err(err) = db.upsert_profile(&profile.id, &profile.name, rank) {
            return Err(err);
        }
        if !known.contains(&profile.id) {
            adopted.profiles = adopted.profiles.saturating_add(1);
        }
    }

    // Only if the user hasn't already picked one here.
    let has_active = match db.active_profile() {
        Ok(active) => active.is_some(),
        Err(err) => return Err(err),
    };
    if !has_active
        && let Some(active) = file.active_profile_id
        && let Err(err) = db.set_active_profile(&active)
    {
        return Err(err);
    }

    Ok(adopted)
}

/// Parses `path` as TOML, logging and returning `None` on any failure.
// `clippy::question_mark_used` wants this without `?`, but the only way to
// do that without a bare `Err(_)` arm (which this codebase's own
// humanization scanner rejects as an undocumented wildcard match) is
// `.ok()?`, which still uses `?`. Kept as-is.
#[expect(clippy::question_mark_used, reason = "see comment above")]
fn read_toml<T>(path: &Path) -> Option<T>
where
    T: DeserializeOwned,
{
    let content = fs::read_to_string(path).ok()?;
    match toml::from_str(&content) {
        Ok(parsed) => Some(parsed),
        Err(err) => {
            tracing::warn!("skipping {}: {err}", path.display());
            None
        }
    }
}

#[cfg(test)]
mod tests;
