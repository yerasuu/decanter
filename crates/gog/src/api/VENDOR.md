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
