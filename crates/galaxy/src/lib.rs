// SPDX-License-Identifier: GPL-3.0-or-later
//! The local Galaxy service: what a game's GOG Galaxy SDK talks to on `127.0.0.1:9977`, answered
//! from what decanter stores instead of from GOG (kb #1461, decisions #1462 and #1512).
//!
//! GOG documents none of the protocol. The messages come from the reverse-engineered, CC0
//! definitions vendored in `proto/`; the framing and the order of calls are what comet's handlers
//! show, not anything seen from a real SDK yet (docs/galaxy-local-service-plan.md, sections 0-2).
//! It serves sign-in, achievements (list, unlock, clear), user stats (list, bounded update) and
//! time played; every other request is answered `NOT_IMPLEMENTED` and counted. Nothing here
//! reaches the network beyond the loopback listener, which serves only programs of the same user
//! and holds every client to the caps and rates of plan 2.4. One [`Host`] serves every game
//! decanter runs at once, each signed in only for as long as its [`Lease`] lives.

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
    reason = "each request's handler is split out to name it, not because it's reused"
)]
#![expect(
    clippy::separated_literal_suffix,
    reason = "`0_u8` is the workspace's spelling; unseparated_literal_suffix asks the opposite"
)]
#![expect(
    clippy::large_stack_frames,
    reason = "tokio's connection futures and prost's derived decoders cost more than the 512-byte threshold"
)]
#![allow(
    clippy::missing_docs_in_private_items,
    reason = "private items get a doc comment when the WHY isn't obvious from the signature, \
              not as a blanket requirement — see CLAUDE.md's comment philosophy"
)]

mod codec;
mod error;
mod handlers;
mod header;
mod host;
mod limits;
mod log_id;
mod peer;
pub mod proto;
mod service;
mod source;

pub use codec::{
    Frame, FrameError, FrameHeader, MAX_HEADER_BYTES, MAX_PAYLOAD_BYTES, PARTIAL_FRAME_TIMEOUT, read_frame, write_frame,
};
pub use error::Error;
pub use header::ResponseHeader;
pub use host::{Account, Host, Lease};
pub use log_id::{LogId, log_id};
pub use service::DEFAULT_PORT;
pub use source::{Achievement, AchievementList, ClientId, GameStore, SourceError, Stat, StatKind, StatValue};
