// SPDX-License-Identifier: GPL-3.0-or-later
//! Thunderstore: its communities (one per game), a community's packages, a package's versions and
//! dependencies, and downloading a version's zip into the game's mods folder for decanter-mods'
//! import, which alone extracts it (docs/mod-platforms-analysis.md, sections 2, 4 and 6).
//!
//! Thunderstore needs no account to read. Its README documents `/api/v1/package/`; the
//! community-scoped listing and `/api/experimental/community/` are known only from secondary
//! sources (the Swagger schema did not render for the analysis), and no terms or rate limits are
//! published. So every answer is parsed tolerantly, an element that does not hold up is skipped and
//! counted in the log, an answer that does not hold up at all is a retryable
//! [`Error::Malformed`], requests are paced and carry a descriptive User-Agent, and the community
//! list and listings are cached on disk inside the library.

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
    reason = "steps of a download or a resolution are split out to name them, not because they're reused"
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

mod community;
mod download;
mod error;
mod http;
mod id;
mod layout;
mod listing;
mod resolve;
mod settings;

pub use community::{Community, Suggestions, communities, suggest};
pub use download::{Downloaded, Staging, Transfer, download, download_plan};
pub use error::Error;
pub use http::Client;
pub use id::{CommunityId, Dependency, PackageId, VersionNumber};
pub use layout::{LEAVE_OUT, Placement, PlacementKind, credits, origin, place};
pub use listing::{Listing, Package, PackageVersion, listing};
pub use resolve::{Installed, Plan, PlanItem, Skipped, installed, plan};
pub use settings::{GameSettings, Layout};

/// Which platform a mod's `mod.toml` origin names.
pub const PLATFORM: &str = "thunderstore";

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

/// Where the community list and the listings are cached: `<root>/decanter/thunderstore/` of the
/// library that holds the game, so the cache moves with it as the mods do.
#[must_use]
#[inline]
pub fn cache_dir(mods: &decanter_mods::GameMods) -> Option<std::path::PathBuf> {
    let root = mods.install().parent()?.parent()?;
    Some(root.join("decanter").join("thunderstore"))
}
