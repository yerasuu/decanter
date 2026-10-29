# Provenance

Based on gog 0.5.0 (gogapi-rs) by nicohman, GPL-3.0.

- Name and version: `gog` 0.5.0
- Author and copyright holder: nicohman <nicohman@demenses.net>, as `authors` in `Cargo.toml`
- Licence: GPL-3.0, as declared in its published `Cargo.toml`. `LICENSE` is the GNU GPL version 3 text; the author
  grants no "or later" option, so it is treated as GPL-3.0-only.
- Upstream: https://git.sr.ht/~nicohman/gogapi-rs
- Taken from the crates.io 0.5.0 package on 2026-10-29. `Cargo.toml` was the normalised one that cargo
  publishes, `src/`, `tests/`, `LICENSE` and `README.md` were byte for byte. They now live in
  `crates/gog`: `src/` is this directory, `tests/lib.rs` is `crates/gog/tests/api.rs`, `README.md` is
  `README.upstream.md`, and the dependencies of `Cargo.toml` are in `crates/gog/Cargo.toml`.
- Upstream shows no release since 2023-02-23 and no commits since 2020-09-10.
- This directory is now maintained here, as the `api` module of `decanter-gog`.
- Not every file here derives from it: the `client/` files marked `GPL-3.0-or-later` without a based-on line
  are decanter's own code, moved in from `decanter-gog`'s other modules (phase 2 of `docs/own-gog-client-plan.md`),
  and owe nothing to upstream.

## Changes made in this repository

| What | Why | Commit |
| --- | --- | --- |
| Workspace member `crates/gogapi`, path dependency `gog` in the root `Cargo.toml`; `license` in `Cargo.toml` spelled `GPL-3.0-only` (SPDX) | Build from this tree instead of crates.io; the licence check expects an SPDX id | refactor(gog): build the gog client from crates/gogapi |
| Crate-level `#![allow(...)]` and a pointer comment at the top of `src/lib.rs` | Keep the code as published while clippy stays clean; credit upstream | refactor(gog): build the gog client from crates/gogapi |
| `rustfmt.toml` with the default `max_width = 100` | The workspace formats at 120 and upstream was formatted at 100; this keeps `cargo fmt --check` clean without reformatting the code | refactor(gog): build the gog client from crates/gogapi |
| Dependencies raised to the latest stable (curl 0.4.51, log 0.4.34, regex 1.13.1, reqwest 0.13.5 with `blocking`, `cookies` and `form`, select 0.6.1, serde and serde_json 1.0.229 and 1.0.154, time 0.3.55); `edition` 2021 to 2024 | Keep the code current and drop the second `reqwest` (0.11) with its `hyper` 0.14, `h2`, `native-tls` and `windows-sys` 0.48 stack from the build; `reqwest` 0.13 made `.form()` opt-in, hence the `form` feature | chore(gog): update the gogapi dependencies |
| `serde` with the `derive` feature and the unused `serde_derive` dependency removed | The code derives with `serde::{Deserialize, Serialize}`, which needs that feature; it only built inside the workspace because another crate enabled it, and `cargo check -p gog` failed on its own | chore(gog): update the gogapi dependencies |
| `style_edition = "2021"` in `rustfmt.toml` | Edition 2024 changes rustfmt's style; this keeps the upstream formatting and `cargo fmt --check` clean without reformatting the code | chore(gog): update the gogapi dependencies |
| `error-chain` 0.12.4 and `select` 0.6.1 stay as they are | Both are at their latest release and unmaintained; the async client of the unification plan (kb #1213) removes them | none |
| Moved from the `gog` crate in `crates/gogapi` into `decanter-gog` as the `api` module (`src/lib.rs` is `mod.rs`, the other files keep their names, `tests/lib.rs` is `crates/gog/tests/api.rs`, `README.md` is `README.upstream.md`); `crate::` paths point at `crate::api::`; the dependencies merged into `crates/gog/Cargo.toml` through `[workspace.dependencies]` | One GOG crate instead of two; the async client of the unification plan (kb #1213) grows beside it | refactor(gog): move gogapi into crates/gog as the api module |
| `time` dropped | Declared upstream, never used: the code only uses `std::time` | refactor(gog): move gogapi into crates/gog as the api module |
| Clippy groups and `rustfmt` skipped on `mod api` in `crates/gog/src/lib.rs`; `rustfmt.toml` beside the tests | `cargo fmt` reads the configuration of the crate root's directory, so `rustfmt.toml` here no longer applies to these files through it; the tests are their own crate root and keep upstream formatting with the same configuration | refactor(gog): move gogapi into crates/gog as the api module |
| The upstream tests marked `#[ignore]` | They log in to GOG with the token at `GOG_TOKEN_PATH` and fail offline; as a workspace member they would run in every `cargo test` | refactor(gog): move gogapi into crates/gog as the api module |
| `SPDX-License-Identifier: GPL-3.0-only` and a based-on line at the top of every `.rs` file (in `mod.rs` they replace the pointer comment); `LICENSE` kept beside them; `license` of `crates/gog/Cargo.toml` is `GPL-3.0-or-later AND GPL-3.0-only` | The crate is mixed (the rest of `decanter-gog` is GPL-3.0-or-later), so each file states its own terms | refactor(gog): attribute and license the api module per file |
| New submodule `client/` (`Client`, `Token`, `Error`, login code exchange, refresh, user data, owned games, filtered products) and `pub mod client;` in `mod.rs`; it restates the workspace's lints for itself in `client/mod.rs` | Phase 1 of `docs/own-gog-client-plan.md`: one async client with typed errors that read HTTP status codes, beside the blocking `Gog` until it replaces it; `Token` keeps the stored JSON format | feat(gog): add an async client core to the api module |
| Licence per file in `client/`: `auth.rs`, `user.rs` and `products.rs` GPL-3.0-only with a based-on line (ported from this module's `token.rs` and `Gog`), `mod.rs`, `http.rs` and `error.rs` GPL-3.0-or-later (from decanter-gog's own code) | Each file states its own terms, as in the rest of this mixed crate | feat(gog): add an async client core to the api module |
| The client core's `client.rs` is `client/http.rs` | So the path does not read `client::client` | feat(gog): add an async client core to the api module |
| `client/` stays under the `#[rustfmt::skip]` of `mod api` and is formatted by hand with this directory's `rustfmt.toml` (`rustfmt --edition 2024 --config-path crates/gog/src/api/ crates/gog/src/api/client/mod.rs`) | Skipping the old files one by one inside `mod.rs` would still let `cargo fmt` reformat `mod.rs` itself, and `#![rustfmt::skip]` as an inner attribute is unstable | feat(gog): add an async client core to the api module |
| `client/catalog.rs` (GPL-3.0-or-later): the `catalog.gog.com` mods request of decanter-gog's `mods.rs`, as `Sessionless::catalog_mods`; `Sessionless` (no session, an access token borrowed per call), the `Api`, `Catalog`, `Cdn`, `ContentSystem` and `GamesDb` hosts, and the answer-from-the-host-asked check in `client/http.rs` | Phase 2 of `docs/own-gog-client-plan.md`: one HTTP client, error type and set of size caps for every GOG host; the cache and the checks of what is kept stay in `mods.rs` | refactor(gog): move the mods catalog into the api client |
