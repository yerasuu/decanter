// SPDX-License-Identifier: GPL-3.0-or-later
//! Amazon Games: the account session, the library listing and a game's file manifest, for now
//! (kb #1520, #1521 and #1611, stages S1 to S3 of docs/amazon-nile-integration-analysis.md).
//!
//! Amazon documents none of this. The wire protocol is the one
//! [nile](https://github.com/imLinguin/nile) speaks and the
//! [community research wiki](https://github.com/Lariaa/GameLauncherResearch/wiki/Amazon-Games)
//! describes, taken as facts only: no code of nile's is here (decision #1518, D1), and every fact
//! is UNVERIFIED against Amazon until the owner's spike (kb #1519). Linux and amazon.com only
//! (D10, D11). Tokens live in the OS keyring (D4); nothing here writes to the library: the caller
//! caches what [`list_games`] returns.

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
    reason = "steps of a login or a renewal are split out to name them, not because they're reused"
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
    reason = "`0_u8` is the workspace's spelling; unseparated_literal_suffix asks the opposite"
)]
#![expect(
    clippy::mod_module_files,
    reason = "directory modules are `mod.rs` across the workspace; self_named_module_files asks the opposite"
)]
#![allow(
    clippy::missing_docs_in_private_items,
    reason = "private items get a doc comment when the WHY isn't obvious from the signature, \
              not as a blanket requirement — see CLAUDE.md's comment philosophy"
)]

mod device;
mod error;
mod http;
mod identity;
mod library;
mod login;
mod manifest;
mod pkce;
mod session;

pub use library::{AmazonGame, Library, Listing, list_games};
pub use login::{PendingLogin, complete_login, extract_code};
pub use manifest::{
    Manifest, ManifestError, ManifestFile, ManifestPath, PathFault, Plan, Sha256Digest, parse_signed_manifest, plan,
};
pub use session::{Account, Deregistration, Live, LiveSession, SessionState, live_session, log_out, session_state};
