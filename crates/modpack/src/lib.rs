// SPDX-License-Identifier: GPL-3.0-or-later
//! Mod packs: a named, ordered list of mod references for one game, mixing providers and local
//! archives (docs/mod-packs-and-search-design.md, sections 3, 5 and 8).
//!
//! A pack is a recipe, not state: [`Pack`] is the file shared and exported, checked as untrusted
//! input wherever it is read; [`resolve`] turns its entries and their edges into the one sequence
//! every machine installs and loads in; [`diff`] compares it with the mods a game has; [`Plan`] is
//! what applying it would do, as data. [`LocalNotes`] is this machine's expendable record of which
//! mods a pack installed. Nothing here reaches the network or knows a provider's types: those stay
//! in the app's glue, which answers [`Availability`] per entry.

#![expect(
    clippy::implicit_return,
    reason = "directly contradicts needless_return from clippy::all, also enabled"
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
    reason = "every error is mapped to its field or meaning first; `?` only carries it up"
)]
#![expect(
    clippy::single_call_fn,
    reason = "steps of a validation or a resolution are split out to name them, not because they're reused"
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
    clippy::shadow_reuse,
    reason = "`let text = text.trim()` names the checked value as the unchecked one was"
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
    clippy::pattern_type_mismatch,
    reason = "matching on references with default binding modes is the workspace's style"
)]
#![expect(
    clippy::impl_trait_in_params,
    reason = "closures are passed as `impl Fn`, which callers never name"
)]
#![expect(
    clippy::large_stack_frames,
    reason = "serde's derived visitors cost more than the 512-byte threshold"
)]

mod diff;
mod error;
mod file;
mod id;
mod local;
mod pack;
mod plan;
mod raw;
mod resolve;

pub use diff::{Diff, EntryDiff, Status, diff};
pub use error::{Error, Field, Reason};
pub use file::{local_file, pack_file};
pub use id::{EntryKey, Md5, PackId, Sha256};
pub use local::{LocalNotes, Note};
pub use pack::{Entry, GameHint, Pack, Source, Track};
pub use plan::{Availability, Plan, Step, StepState};
pub use resolve::{
    Contradiction, Cycle, Dependency, Edge, EdgeKind, Move, Resolution, ResolveError, Strength, resolve,
};

/// The only `schema` this decanter reads and writes.
pub const SCHEMA: u32 = 1;
/// A pack file larger than this is refused before it is parsed (design, section 8.2).
pub const MAX_PACK_BYTES: usize = 1 << 20;
/// Entries one pack may hold (design, open question 16).
pub const MAX_ENTRIES: usize = 500;
/// `requires` and `after` references in all of a pack (design, open question 16).
pub const MAX_EDGES: usize = 2000;

pub type Result<T> = core::result::Result<T, Error>;
