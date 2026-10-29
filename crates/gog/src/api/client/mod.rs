// SPDX-License-Identifier: GPL-3.0-or-later
//! The async client on the workspace's `reqwest`, which replaces the blocking [`super::Gog`] one
//! flow at a time (see `docs/own-gog-client-plan.md`). Unlike the rest of the `api` module it
//! is decanter's own code and keeps the workspace's lints, restated here because the module as a
//! whole does not inherit them.
#![forbid(unsafe_code)]
#![warn(
    dead_code,
    clippy::all,
    clippy::pedantic,
    clippy::restriction,
    clippy::nursery
)]
#![allow(
    clippy::blanket_clippy_restriction_lints,
    reason = "the workspace allows it too: restriction is enabled as a group on purpose"
)]
#![deny(
    clippy::redundant_clone,
    clippy::clone_on_ref_ptr,
    clippy::unnecessary_to_owned,
    clippy::unnecessary_cast,
    clippy::let_unit_value,
    clippy::unnecessary_wraps,
    clippy::trait_duplication_in_bounds,
    clippy::extra_unused_type_parameters,
    clippy::multiple_bound_locations,
    clippy::unnecessary_filter_map,
    clippy::needless_collect,
    clippy::manual_flatten,
    clippy::flat_map_option,
    clippy::mutex_atomic,
    clippy::mutex_integer,
    clippy::format_in_format_args,
    clippy::to_string_in_format_args,
    clippy::string_lit_as_bytes,
    clippy::uninlined_format_args,
    clippy::derivable_impls,
    clippy::partial_pub_fields,
    clippy::match_like_matches_macro,
    clippy::match_same_arms,
    clippy::match_wild_err_arm,
    clippy::panic,
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::unreachable,
    clippy::todo,
    clippy::unimplemented
)]

mod auth;
mod error;
mod http;
mod products;
mod user;

pub use auth::{Token, CLIENT_ID, CLIENT_SECRET, REDIRECT_URI};
pub use error::Error;
pub use http::Client;
pub use products::ProductsPage;
pub use user::User;
