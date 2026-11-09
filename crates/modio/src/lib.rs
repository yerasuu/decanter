// SPDX-License-Identifier: GPL-3.0-or-later
//! mod.io: which mod.io game a library game is, that game's mods, a mod's files and dependencies,
//! mod.io's terms of use, and downloading one modfile into the game's mods folder for
//! decanter-mods' import, which alone extracts it (docs/mod-platforms-analysis.md, sections 3, 4
//! and 6).
//!
//! decanter ships no API key of its own (decision #1629): every request carries the user's key,
//! read from the keyring and held in a [`decanter_core::secrets::Secret`], and no error or log
//! carries it or a URL it is in. mod.io serves a game to a third-party tool only while the game's
//! own record has `api_access_options` bit 1 set, so that record is read and checked before its
//! mods are; whether a user's key is enough for a game that did not set it is not documented
//! (analysis, section 8), so such a game is refused rather than tried. A 429 blocks every request
//! until its `retry-after` has passed, since the reference warns that ignoring them can revoke
//! the key.

#![expect(
    clippy::implicit_return,
    reason = "directly contradicts needless_return from clippy::all, also enabled"
)]
#![expect(
    clippy::question_mark_used,
    reason = "every error is mapped to its call or meaning first; `?` only carries it up"
)]
#![expect(
    clippy::pub_with_shorthand,
    clippy::redundant_pub_crate,
    clippy::pub_use,
    reason = "this crate uses the pub(crate) shorthand and re-exports its modules' types, as the others do"
)]
#![expect(
    clippy::std_instead_of_alloc,
    reason = "a desktop crate on std, with no no_std target to justify alloc::"
)]
#![expect(
    clippy::arbitrary_source_item_ordering,
    reason = "items are ordered by what makes sense to read together, not alphabetically"
)]
#![expect(
    clippy::single_call_fn,
    reason = "steps of a download or an answer's parsing are split out to name them, not because they're reused"
)]
#![expect(
    clippy::large_stack_frames,
    reason = "serde's derived visitors and reqwest's futures cost more than the 512-byte threshold"
)]
#![expect(
    clippy::absolute_paths,
    reason = "one-off calls read clearer qualified at the call site than imported for a single use"
)]
#![expect(
    clippy::separated_literal_suffix,
    reason = "`0_u64` is the workspace's spelling; unseparated_literal_suffix asks the opposite"
)]
#![allow(
    clippy::missing_docs_in_private_items,
    reason = "private items get a doc comment when the WHY isn't obvious from the signature, \
              not as a blanket requirement — see CLAUDE.md's comment philosophy"
)]

mod download;
mod error;
mod files;
mod games;
mod http;
mod id;
mod mods;
mod settings;
mod terms;

pub use download::{Downloaded, Staging, Transfer, download};
pub use error::Error;
pub use files::{ModFile, Virus};
pub use games::{Access, Game, check_access, game, probe, search_games};
pub use http::{Client, is_key_shaped};
pub use id::{FileId, GameId, ModioModId};
pub use mods::{Dependency, Detail, ModsPage, PAGE_SIZE, RemoteMod, detail, fetch_mod, search_mods};
pub use settings::GameSettings;
pub use terms::{Terms, agreed, forget_agreement, record_agreement, terms};

/// Which platform a mod's `mod.toml` origin names.
pub const PLATFORM: &str = "modio";

/// Where the user gets a key of their own, linked from Ajustes.
pub const KEY_PAGE: &str = "https://mod.io/me/access";

pub type Result<T> = core::result::Result<T, Error>;

/// What `mod.toml` records of a mod downloaded from mod.io, read back for an update check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct Origin {
    pub game: GameId,
    pub mod_id: ModioModId,
    pub file: FileId,
}

impl Origin {
    #[must_use]
    #[inline]
    pub const fn new(game: GameId, mod_id: ModioModId, file: FileId) -> Self {
        Self { game, mod_id, file }
    }

    /// The origin `mod.toml` keeps: `package` is `<game>-<mod>` and `version` is `<file>`, then
    /// `-<version>` when the modfile names one that fits an origin token, since decanter-mods'
    /// origin has no field for the file and the version string is free text on mod.io.
    ///
    /// # Errors
    ///
    /// When a field can't be an origin's, which numeric ids and a checked host never make.
    #[inline]
    pub fn to_mod_origin(self, version: Option<&str>, host: &str) -> Result<decanter_mods::ModOrigin> {
        let package = format!("{}-{}", self.game, self.mod_id);
        let file = self.file.to_string();
        let stored = version
            .map(|text| id::origin_token(text, 100))
            .filter(|text| !text.is_empty())
            .map_or_else(|| file.clone(), |text| format!("{file}-{text}"));
        Ok(decanter_mods::ModOrigin::new(PLATFORM, &package, &stored, host)?)
    }

    /// The mod.io ids of a mod's origin, `None` for another platform's or one that does not parse.
    #[must_use]
    #[inline]
    pub fn from_mod_origin(origin: &decanter_mods::ModOrigin) -> Option<Self> {
        if origin.platform != PLATFORM {
            return None;
        }
        let (game, mod_id) = origin.package.split_once('-')?;
        let file = origin
            .version
            .split_once('-')
            .map_or(origin.version.as_str(), |(file, _)| file);
        Some(Self {
            game: game.parse().ok()?,
            mod_id: mod_id.parse().ok()?,
            file: file.parse().ok()?,
        })
    }

    /// The version the modfile named, as `mod.toml` keeps it.
    #[must_use]
    #[inline]
    pub fn version_of(origin: &decanter_mods::ModOrigin) -> Option<&str> {
        origin.version.split_once('-').map(|(_, version)| version)
    }
}

/// Where mod.io's terms agreement is kept: `<root>/decanter/modio/` of the library that holds the
/// game, so it moves with the library as the mods do.
#[must_use]
#[inline]
pub fn data_dir(mods: &decanter_mods::GameMods) -> Option<std::path::PathBuf> {
    let root = mods.install().parent()?.parent()?;
    Some(root.join("decanter").join("modio"))
}
