# One GOG API: gogapi and decanter's own clients unified

Supersedes the first version of this plan (rewrite from scratch, rejected in kb #1211). Decisions of the owner,
2026-10-29: the `gog` 0.5.0 crate is incorporated as `crates/gog/src/api` (#1211, #1212); the unified API lives **in
`crates/gog/src/api` with the licence kept per file**; and it moves to **async on the project's `reqwest` 0.13**, dropping
the blocking client and the second `reqwest`.

## Status (2026-10-30)

Phases 1 to 3 are done and phase 4 is as built below; the sections after this one are the plan as it was written,
and "Where things are today" describes the state before phase 1.

- Phase 1 to 3: `crates/gog/src/api/client` holds the one async `Client` on `reqwest` 0.13 (`auth`, `user`, `products`,
  `achievements`, `orders`) and `Sessionless` for the calls that bear a borrowed access token (`catalog`, `gamesdb`,
  `galaxy_products`, `content`), with the typed `Error`. `with_client`, `spawn_blocking`, the blocking `Gog`,
  `error-chain`, `select`, `curl`, `regex`, `log` and `reqwest` 0.11 are gone.
- Licence: the four files that derived from gog 0.5.0 were rewritten clean-room from
  `docs/gog-api-behaviour-spec.md` (kb #1228, decision #1225), so no gogapi-rs code remains and `decanter-gog` is
  `GPL-3.0-or-later` as a whole; the "Licence per file" section below is superseded.
- Phase 4: the "unused methods" were the ~30 methods of the blocking `Gog` that decanter never called. None was kept
  or specified beyond the ones decanter used (token exchange and refresh, user data, owned games, filtered products,
  achievements); the rest were never ported and went with `gog.rs`. `VENDOR.md` says so. A search for items of
  `crates/gog` with no caller (rustc `dead_code`, and every `pub` name looked up across `app/` and `crates/`) found
  nothing more to remove, and no dependency of `crates/gog` or `[workspace.dependencies]` is unused.
- The provenance record is `crates/gog/src/api/VENDOR.md` (the plan's `crates/gogapi/VENDOR.md` was its first
  location).

## Where things are today

- `crates/gog/src/api` (nicohman's code, GPL-3.0-only, blocking `reqwest` 0.11, `error-chain`): `Token` and the login code
  exchange, `Gog` (user data, owned games, filtered products, ~30 methods), `ProductDetails`. decanter uses 5 of them.
- `crates/gog` (decanter-gog, 5900 lines, GPL-3.0-or-later) already talks to GOG with its own async `reqwest` 0.13 for:
  `session.rs` (token refresh at `auth.gog.com`, `userData.json`, status codes), `product.rs` (`api.gog.com/products`),
  `gamesdb.rs`, `orders.rs`, `mods.rs` (catalog), `depot.rs` and `install/` (`content-system`, CDN), `steam_export.rs`,
  `cloud_saves.rs`.
- The wrapper between the two: `crates/gog/src/lib.rs` `with_client` runs every gogapi call in `spawn_blocking` behind a
  per-account lock, and `session.rs` exists partly because the old client **never looks at HTTP status codes**: a 503
  page and a revoked token both reach the caller as "expected value at line 1 column 1".
- Two `reqwest` versions (0.11 and 0.13) and two error styles (`error-chain` and `thiserror`/`anyhow`) in one binary.

## Target

One crate, one `Client`: async on `reqwest` 0.13, one token and session type, one error type that tells apart
"GOG rejected the session", "GOG could not be reached" and "the answer was malformed" (what `session.rs` does today), one
place for base URLs and edge validation, and each GOG host's calls in its own module (`auth`, `user`, `products`, `gamesdb`,
`orders`, `catalog` (mods), `content` (builds, depots), `achievements`). `decanter-gog` keeps what is decanter's (install
flow, cloud saves, library state, hooks) and calls that client.

## Licence per file

- Files derived from nicohman's crate keep `GPL-3.0-only` and their attribution; each change is one row in
  `crates/gogapi/VENDOR.md`.
- A module derived only from `decanter-gog`'s own code carries `SPDX-License-Identifier: GPL-3.0-or-later` in its header; one derived from the crate's code stays `GPL-3.0-only`, and a mixed one takes the stricter, as
  psport marks per file. `LICENSES.md` and `LICENSES.json` list the crate as mixed with the per-file rule; the combined
  binary is still GPL-3.0 (kb #1073) until the last derived file is gone.
- The token JSON format stored in the keyring (`gog::token::Token`, serde) does not change, so nobody is logged out.

## Phases (each one a commit or two, builds clean, tested by hand with the owner's account before the next)

1. **Async core in gogapi**: `Client` on `reqwest` 0.13 with the one token, refresh behind a single lock, the typed error
   with HTTP-status handling, and the calls decanter uses (login code exchange, refresh, user data, owned games, filtered
   products). Added next to the old blocking client; callers unchanged.
2. **Move the clients in**: `product`, `gamesdb`, `orders`, `catalog` (mods, #1203), `content` and the session check
   from `decanter-gog` into gogapi modules over the shared client, with `decanter-gog` calling them.
3. **Switch the flows**: auth, session, library, orders, updates, install and verify use the async client; remove
   `with_client`, `spawn_blocking` and the per-account lock (the refresh lock stays); remove the blocking `Gog`, `error-chain`
   and `reqwest` 0.11.
4. **Clean up**: unused methods of the old crate that nothing needs are listed in `VENDOR.md` (kept, specified, or removed),
   docs and manifests updated.

## Risks

- Login and refresh are delicate: phase 1 keeps the stored token format and the owner tests login, refresh and a
  revoked-session case by hand.
- The endpoints are not officially documented: typed errors and validation at the edge, and a manual check per flow.
- No agent connects to GOG: everything that needs the account is checked by the owner.
