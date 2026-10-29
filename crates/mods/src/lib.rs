//! Third-party mods over an installed game, applied without writing to the install
//! (docs/third-party-mods-design.md, decision #1205).
//!
//! A game's mods live beside its install, in `<root>/decanter/mods/<store>/<game id>/`: one folder
//! per mod with its `mod.toml`, shared by every account that has the game, and per account the set
//! it plays with (`profiles/<account>.toml`) and its play tree (`play/<account>/`), a mirror of the
//! install made of real directories and one link per file. The game starts from the play tree, so
//! verify, repair and update keep reading and writing only `common/<InstallDir>`.
//!
//! Everything read from these folders (`mod.toml`, set files, the play tree's record) and every
//! folder a user picks is checked here, where it enters; nothing here reaches the network.

#![expect(
    clippy::implicit_return,
    reason = "directly contradicts needless_return from clippy::all, also enabled"
)]
#![expect(
    clippy::large_stack_frames,
    reason = "serde's derived visitors and the walks' path buffers cost more than the 512-byte threshold"
)]
#![expect(
    clippy::arbitrary_source_item_ordering,
    reason = "items are ordered by what makes sense to read together, not alphabetically"
)]
#![allow(
    clippy::missing_docs_in_private_items,
    reason = "private items get a doc comment when the WHY isn't obvious from the signature, \
              not as a blanket requirement — see CLAUDE.md's comment philosophy"
)]
#![expect(
    clippy::question_mark_used,
    reason = "every error is mapped to its path or meaning first; `?` only carries it up"
)]
#![expect(
    clippy::single_call_fn,
    reason = "steps of an import or a reconcile are split out to name them, not because they're reused"
)]
#![expect(
    clippy::pub_with_shorthand,
    clippy::redundant_pub_crate,
    clippy::field_scoped_visibility_modifiers,
    clippy::pub_use,
    reason = "this crate uses the pub(crate) shorthand and re-exports its modules' types, as the others do"
)]
#![expect(
    clippy::shadow_reuse,
    reason = "`let entry = entry.map_err(..)?` names the checked value as the unchecked one was"
)]
#![expect(
    clippy::separated_literal_suffix,
    reason = "`0_u64` is the workspace's spelling; unseparated_literal_suffix asks the opposite"
)]
#![expect(
    clippy::absolute_paths,
    reason = "one-off calls read clearer qualified at the call site than imported for a single use"
)]
#![expect(
    clippy::std_instead_of_alloc,
    reason = "a desktop crate on std, with no no_std target to justify alloc::"
)]

mod error;
mod files;
mod game;
mod import;
mod manifest;
mod merge;
mod path;
mod set;
mod tree;

pub use error::Error;
pub use game::{GameMods, ModConflicts, Store};
pub use manifest::{ModFile, ModManifest};
pub use path::{ModId, ModPath};
pub use tree::Prepared;

/// The owner's cap (design, open question 8), since a texture pack can be larger than 20 gigabytes.
pub const MAX_MOD_BYTES: u64 = 64 << 30;
/// The owner's cap (design, open question 8).
pub const MAX_MOD_FILES: usize = 200_000;
/// What an import leaves free on the library's disk, so a mod never fills it.
pub const FREE_SPACE_MARGIN: u64 = 1 << 30;

pub type Result<T> = core::result::Result<T, Error>;
