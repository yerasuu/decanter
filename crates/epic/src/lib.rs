// SPDX-License-Identifier: GPL-3.0-or-later
//! Epic Games: the client of the official launcher's web services, which decanter speaks as that
//! launcher (docs/epic-legendary-integration-analysis.md, option R of D4). So far it signs an
//! account in from the browser and keeps its session in the OS keyring.
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
#![allow(
    clippy::missing_docs_in_private_items,
    reason = "private items get a doc comment when the WHY isn't obvious from the signature, \
              not as a blanket requirement — see CLAUDE.md's comment philosophy"
)]

mod error;
mod hosts;
mod http;
mod identity;
mod login;
mod session;
mod token;

pub use error::CallError;
pub use hosts::Host;
pub use login::{complete_login, login_url};
pub use session::{Account, SessionState, check_session, log_out};
