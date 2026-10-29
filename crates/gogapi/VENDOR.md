# Provenance

Based on gog 0.5.0 (gogapi-rs) by nicohman, GPL-3.0.

- Name and version: `gog` 0.5.0
- Author and copyright holder: nicohman <nicohman@demenses.net>, as `authors` in `Cargo.toml`
- Licence: GPL-3.0, as declared in `Cargo.toml`. `LICENSE` is the GNU GPL version 3 text; the author
  grants no "or later" option, so it is treated as GPL-3.0-only.
- Upstream: https://git.sr.ht/~nicohman/gogapi-rs
- Taken from the crates.io 0.5.0 package on 2026-10-29. `Cargo.toml` is the normalised one that cargo
  publishes, `src/`, `tests/`, `LICENSE` and `README.md` are byte for byte.
- Upstream shows no release since 2023-02-23 and no commits since 2020-09-10.
- This directory is now maintained here, as part of decanter.

## Changes made in this repository

| What | Why | Commit |
| --- | --- | --- |
| Workspace member `crates/gogapi`, path dependency `gog` in the root `Cargo.toml`; `license` in `Cargo.toml` spelled `GPL-3.0-only` (SPDX) | Build from this tree instead of crates.io; the licence check expects an SPDX id | refactor(gog): build the gog client from crates/gogapi |
| Crate-level `#![allow(...)]` and a pointer comment at the top of `src/lib.rs` | Keep the code as published while clippy stays clean; credit upstream | refactor(gog): build the gog client from crates/gogapi |
| `rustfmt.toml` with the default `max_width = 100` | The workspace formats at 120 and upstream was formatted at 100; this keeps `cargo fmt --check` clean without reformatting the code | refactor(gog): build the gog client from crates/gogapi |
