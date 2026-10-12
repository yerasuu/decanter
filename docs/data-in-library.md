# Keeping decanter's data inside the library

Design principle: everything decanter stores lives inside the profile's
library root (`<root>`, e.g. `/home/deck/data/Decanter/steamapps`);
outside it only the pointers that can't be avoided. This page records
what was where on 2026-10-12, the plan, and (at the end) what was
implemented.

## Already inside the library

- Game files, manifests and prefixes (`<root>/compatdata/<account>/<game>`).
- Install staging (`<root>/downloading/`).
- Proton logs (in the prefix) and the Galaxy hook's achievement log
  (`<prefix>/decanter-hook.jsonl`).
- Everything PS: `<root>/psport/` (games, ports, toolchain with
  nix-portable, settings, mods, profiles, layouts, run logs).
- Per-game desktop entries and icons: `<root>/decanter/{desktop,icons}`.

## Still outside, to move in

| Item | Today | Size | Target |
|---|---|---|---|
| Database `links.db` (profiles, accounts, game cache, achievements, playtime, collections, settings) | `~/.local/share/decanter/links.db` (`crates/core/src/db.rs` `default_db_path`) | 676K | `<root>/decanter/decanter.db` |
| Art cache | `~/.local/share/decanter/art/` (`app/src/art.rs`) | 11M | `<root>/decanter/art/` |
| `umu-run` | `~/.local/share/decanter/umu-run` (`crates/runtime/src/umu.rs`) | 412K | `<root>/decanter/bin/umu-run-<version>` |
| Default library | `~/.local/share/decanter/steamapps/` (`Library::default_path`) | empty | retired; only an onboarding suggestion |

## Outside on purpose

- `~/.config/decanter/libraries.toml`: the bootstrap pointer (known
  libraries, active library and profile), to be added.
- `~/.local/share/applications/decanter/`: one symlink per game; Plasma
  only reads its application folders.
- Keyring (`dev.decanter.Decanter`): GOG tokens and the SteamGridDB key;
  secrets don't belong in a folder that gets copied around.
- Steam's own files: `shortcuts.vdf`, `grid/`, `compatibilitytools.d`.
- umu's runtime and caches (`~/.local/share/umu`, `~/.cache/umu-protonfixes`):
  shared with Steam, Heroic and Lutris.
- KDE's `ksycoca`, systemd scopes, the runtime lock: transient or system-owned.

## Leftovers that nothing uses any more

- `~/.local/share/decanter/{hsts-storage.sqlite,mediakeys,storage,webview,staging}`
  and `~/.cache/decanter/` (78M): the old WebKit build and the old staging.
- Standalone bbport/psport/shadPS4 data: `~/.local/share/bbport`,
  `~/.local/share/psport`, `~/.config/bbport-launcher`, `~/.cache/bbport`,
  `~/.local/share/shadPS4` (544M), `~/.local/share/shadPS4QtLauncher` (33M).
  Decanter uses `<root>/psport/bbport`; whether the shadPS4 folders came
  from decanter or a standalone shadPS4 isn't known.

## Plan

1. **Leftovers**: remove them after confirmation (no code change).
2. **Bootstrap and database path**: `crates/core/src/bootstrap.rs`
   (`libraries.toml`, a `DECANTER_LIBRARY` override, an exclusive/shared
   lock in `$XDG_RUNTIME_DIR`); `db_path(root)` replaces
   `default_db_path()`; startup never creates a configured root (a missing
   library shows "library unavailable").
3. **One-time migration**, under the exclusive lock and only with no game
   running: copy `links.db` per root with `VACUUM INTO`, drop rows of
   profiles that belong to another root, `PRAGMA integrity_check` and row
   counts, rename into place; move `umu-run`; write `libraries.toml`
   atomically as the commit point; keep `~/.local/share/decanter`
   untouched with a `MIGRATED-TO` note (rollback: delete
   `libraries.toml`).
4. **Remaining files**: art cache and `umu-run` under the root;
   `decanter launch --library <root>` in the per-game desktop entries.
5. **Multiple libraries**: profile switcher across libraries; moving a
   library moves its database, updates the pointer and re-points the
   per-game symlinks.

Trade-offs: an account in two libraries shares one keyring token (logging
out in one signs out the other); SQLite locking is unreliable on network
filesystems (warn when the root is on one); an older decanter started
after the migration keeps using the old database.

## Implemented (2026-10-12)

All five phases are on `develop`: `bootstrap.rs` (`libraries.toml`,
`--library`, `DECANTER_LIBRARY`, the runtime lock, the "library
unavailable" screen), `migrate.rs` (grouping by the same folder on disk,
`VACUUM INTO`, integrity and row checks, `.part` copies, a newer library
database never replaced by an older copy), the art cache and `umu-run`
under the root, `--library` in per-game desktop entries, the old-data
button in Storage, the cross-library profile switcher (refused while a
game runs or an install is queued) and library moves that carry the
database, the pointer and the per-game symlinks. Not done: moving a single
profile to another library. Verified on temporary copies only; the first
real migration happens on the next start of a build that includes it.
