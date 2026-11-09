// SPDX-License-Identifier: GPL-3.0-or-later
//! Epic Games: the client of the official launcher's web services, which decanter speaks as that
//! launcher (docs/epic-legendary-integration-analysis.md, option R of D4). It signs an account in
//! from the browser, keeps its session in the OS keyring, lists its library, and installs,
//! verifies, updates and uninstalls its games and builds their offline command line; only
//! [`install`], [`repair`], [`uninstall`] and [`discard`] write to the library.
//!
//! Epic documents none of this. The protocol is the one docs/epic-api-behaviour-spec.md describes,
//! written from legendary-gl 0.21.1 as facts only: no code of legendary's is here (decisions #1225,
//! #1541 and #1617), and every fact is UNVERIFIED against Epic until the owner's own tests. Every
//! host lives in [`Host`]'s table, and every call ends in a [`CallError`] that keeps a refused
//! session apart from a moved, unreachable or stalled endpoint.

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
    clippy::absolute_paths,
    reason = "one-off calls read clearer qualified at the call site than imported for a single use"
)]
#![expect(
    clippy::arbitrary_source_item_ordering,
    reason = "items are ordered by what makes sense to read together, not alphabetically"
)]
#![expect(
    clippy::single_call_fn,
    reason = "steps of a call or a renewal are split out to name them, not because they're reused"
)]
#![expect(
    clippy::large_stack_frames,
    reason = "serde's derived visitors and reqwest's builders and futures cost more than the 512-byte threshold"
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

mod error;
mod hosts;
mod http;
mod identity;
mod install;
mod launch;
mod library;
mod login;
mod manifest;
mod session;
mod token;
mod updates;

pub use error::CallError;
pub use hosts::Host;
pub use install::{
    BrokenFile, BrokenReason, Target, VerifyReport, cancel, discard, install, installed_manifest, pause,
    pause_for_quit, repair, unfinished_downloads, uninstall, verify,
};
pub use launch::{OfflineLaunch, offline_arguments};
pub use library::{AssetList, CatalogRegion, EpicGame, LaunchAttributes, Library, Listing, list_games};
pub use login::{complete_login, login_url};
pub use manifest::{Manifest, ManifestError, ManifestFile, Sha1Digest, parse_manifest};
pub use session::{Account, SessionState, check_session, log_out};
pub use updates::check_update;
