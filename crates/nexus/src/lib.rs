// SPDX-License-Identifier: GPL-3.0-or-later
//! Nexus Mods with the user's own personal API key: checking the key, which Nexus game a library
//! game is, a game's latest, trending and updated mods, a mod's files, and for Premium accounts
//! downloading one into the game's mods folder for decanter-mods' import, which alone extracts it
//! (docs/mod-platforms-analysis.md, sections 1, 4 and 6).
//!
//! decanter registers no application with Nexus and ships no key of its own (decision #1629): each
//! user pastes a personal key, kept in the OS keyring. The acceptable use policy, the rate limits
//! and the `Application-Name`/`Application-Version` headers are documented; the v1 reference was
//! unreachable for the analysis, so every endpoint, field and rate-limit header here is secondary.
//! So every answer is parsed tolerantly, an element that does not hold up is skipped and counted
//! in the log, an answer that does not hold up at all is a retryable [`Error::Malformed`], and
//! nothing is asked of Nexus without a user action: no polling, update checks only on a button.

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
    reason = "steps of a download or a check are split out to name them, not because they're reused"
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

mod account;
mod download;
mod error;
mod files;
mod games;
mod http;
mod id;
mod key;
mod listing;
mod origin;
mod settings;

pub use account::{Account, validate};
pub use download::{Downloaded, Staging, Transfer, download, download_link};
pub use error::Error;
pub use files::{Category, FileList, NexusFile, Update, files, update};
pub use games::{Game, Suggestions, games, suggest};
pub use http::{Client, Remaining, remaining};
pub use id::{FileId, GameDomain, NexusModId, mod_page, parse_mod_reference};
pub use key::{forget_key, load_key, save_key};
pub use listing::{Feed, ModInfo, feed, mod_info};
pub use origin::{NexusOrigin, origin};
pub use settings::GameSettings;

/// Which platform a mod's `mod.toml` origin names.
pub const PLATFORM: &str = "nexus";

pub type Result<T> = core::result::Result<T, Error>;

/// Whether a cached answer may be used as it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Refresh {
    /// The cache while it is younger than its time to live.
    IfStale,
    /// Always ask again: the user pressed refresh.
    Now,
}

/// Where the game list is cached: `<root>/decanter/nexus/` of the library that holds the game, so
/// the cache moves with it as the mods do.
#[must_use]
#[inline]
pub fn cache_dir(mods: &decanter_mods::GameMods) -> Option<std::path::PathBuf> {
    let root = mods.install().parent()?.parent()?;
    Some(root.join("decanter").join("nexus"))
}
