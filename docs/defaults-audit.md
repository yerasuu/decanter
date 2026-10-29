# Defaults and first-run audit

Audit for #1259 under decision #1258 (2026-10-29): decanter configures itself with what is optimal
for the system, avoids first-load problems, repairs what it manages or says so clearly, and never
fails silently. Read from `develop` at 854da61. Columns:

- **Now**: what decanter does today (file that decides it).
- **System signal**: what the system already reports that could decide it.
- **Proposed**: the automatic behaviour.
- **Risk**: of the proposed change.
- **Owner**: `no` (can go in as a small fix), `propose` (design, storage or dependency choice for
  `kb propose`), `optimal` (nothing to do).

## App settings (`crates/core/src/db/settings.rs`, `db/game_options.rs`)

| Setting | Now | System signal | Proposed | Risk | Owner |
|---|---|---|---|---|---|
| UI language | Saved choice, else the system locale's region then language, else `es` (`app/src/i18n.rs`) | `platform::system_locale()` | Already follows the system | - | optimal |
| Close to tray | Off unless set; only offered where the capability table has a tray (`main.rs`, `Feature::Tray`) | Tray host present (StatusNotifier watcher) | Keep off: closing the window quitting is what a user expects; the capability already hides it where there is no tray | - | optimal |
| Steam export (`shortcuts.vdf`) | On by default (`steam_export_enabled`) | `steam::steam_root()`, `local_user_id()` | Already gated on a Steam install; no change | - | optimal |
| Close-game shortcut / pad chord | On by default | Capability `PadClose` | Already capability-gated | - | optimal |
| Default Proton | Unset: `Proton::Latest`, umu resolves UMU-Proton (`core::proton_or_default`) | umu's own resolution | Already the best general default | - | optimal |
| MangoHud / GameMode defaults | Off library-wide, per-game override (`game_tool_default`) | `mangohud_installed()` (Vulkan implicit layer dirs), `gamemoded()` (binary + D-Bus service) | Keep off (they change how every game looks / how the machine runs); missing-but-on already starts without them and logs a warning (`app/src/game_tools.rs`). A visible note in the game's options when on but not installed would help | Low | no (UI note) |
| MangoHud autostart log | Off | - | Keep | - | optimal |
| psport channel | `release` unless `testing` (`psport_testing`) | Port CI publishing builds | Already: a port without published builds is always Testing | - | optimal |
| Raven section | Off (`raven_enabled`) | Capability `Raven` | Opt-in feature, keep | - | optimal |
| Debug mode / log dir | Off; dir defaults inside the library | - | Keep | - | optimal |
| Shader cache location | `<folder>/decanter/cache` per library folder unless chosen (`app/src/cache.rs`, `default_cache_root`) | Free space of the folder's disk (`disk_usage`) | Keep next to the games (same disk, follows moves) | - | optimal |
| Tokio workers | Fixed 2 (`main.rs`) | `available_parallelism()` | Keep: the runtime only waits on network and disk (comment in `main.rs`) | - | optimal |
| Windows port build jobs | `available_parallelism()` capped by `MAX_JOBS` (`winbuild/mod.rs`) | Cores, RAM | Also cap by RAM (compilers' memory) instead of a fixed cap | Medium | propose |

## PS section (`crates/ps/src/bbport/groups.rs`, `app/src/ps_play/launch.rs`)

| Setting | Now | System signal | Proposed | Risk | Owner |
|---|---|---|---|---|---|
| Port menu language | Follows decanter's UI language (`menu_language: rust_i18n::locale()`) until picked in the menu | UI language | - | - | optimal |
| Game (system) language | Constant `1` (English) when unset (`bbport/mod.rs`, `settings.text("language").unwrap_or("1")`) | decanter's UI language (es → `3`, fr → `2`, de → `4`, it → `5`, ru → `8`, ja → `0`) | Default to the UI language's PS4 code when there is one, English otherwise; the settings table shows it as the current value | Low (changes the text language of games never configured) | propose |
| Output resolution | Constant `1920x1080` | Display size (GPUI display bounds, already read for the aspect ratio) | Nearest listed resolution not above the display's; never above 1080p on an integrated GPU unless chosen | Medium (4K output on a weak GPU) | propose |
| Aspect ratio | `auto`: the display's (`nearest_aspect_ratio`, GUI); none headless | Display size | - | - | optimal |
| Present mode | `Mailbox` | Vulkan surface present modes | psport falls back when Mailbox is missing (psport's side); nothing for decanter | - | optimal |
| Upscaler | `fsr3`; DLSS listed with a note | Vulkan capabilities reported by psport (`features`) | Offer DLSS only when the build reports it usable, by capability, never vendor id (decision #361) | Low | propose (psport feature list) |
| FPS mode / limit | `uncap` / no limit | Display refresh rate | Keep: the port paces itself; a refresh-based limit belongs to psport's resource management (#1250) | - | optimal |
| VRAM / memory budgets | psport's own (decision #1250) | Driver-reported budgets | psport, not decanter | - | optimal |
| Port settings file unreadable | Read as defaults with no log line; a save then overwrote the file with defaults plus one change (`LauncherSettings::load`, `values::save`) | - | Log it; a save fails and shows the error instead of overwriting | Low | done (commit 2) |

## Libraries, profiles and storage

| Item | Now | System signal | Proposed | Risk | Owner |
|---|---|---|---|---|---|
| Default library root | `platform::default_library_dir()`, made by the first profile (`core::create_profile`) | Free space per mount | Keep; the first-run form could show the free space of the default and of the largest writable disk | Low | propose |
| Profile library path equal to its database's library | Stored spelled out (a GUI library move writes it); goes stale when the folder is moved outside decanter, then `ps run` says "no game" (j261, #1257) | `bootstrap::active_root()` | Unset it at startup and after a move: unset already means "the library this database is in" and follows the folder | Low (same resolution) | done (commit 1) |
| Profile library path missing (stale or unplugged) | Read as an empty library: `ps run` says "no game with the ID", the PS section makes `psport/games` under the missing path (`state/ps/mod.rs` `create_dir_all(&games)`), recreating the stale folder or filling an unplugged disk's mount point | `Path::is_dir()` | Report "the profile's library (<path>) isn't there" in `ps run`, `ps report` and the PS section; never create folders under a missing primary root; offer a repair in Ajustes › Cuenta (pick the new place, or point to a known root) | Low for the message, medium for the repair | #1257 (message); propose (repair UI) |
| Extra library folders unplugged | Skipped, listed as offline (`offline_library_folders`) | `is_dir()` | - | - | optimal |
| Active library's disk missing | `views/unavailable.rs`; never creates the root | `bootstrap::unavailable_root()` | - | - | optimal |
| `libraries.toml` missing, legacy `links.db` present | One-time migration into the library (`migrate::run`) | - | - | - | optimal |
| `libraries.toml` missing, no legacy db | First-run screen makes the library | - | - | - | optimal |
| `libraries.toml` unreadable / corrupt | `Location::Unconfigured` with an error log only: the GUI shows the first-run screen as if nothing existed, and creating a profile then fails on the same file (`bootstrap::activate`) | Library roots on disk with `decanter/decanter.db` | Show a clear error naming the file, and rebuild it from the roots it can find (the default root and the last good copy) with the user's confirmation | Medium (touches the pointer file) | propose |
| `known_roots()` read error | Silently empty (`.ok().flatten()`) | - | Covered by the row above once that error is shown | - | - |
| Database from an older version | Column adds and cache-table rebuilds in `Db::open` | - | - | - | optimal |
| Database from a newer version | No schema version: an older decanter opens it as is | `PRAGMA user_version` | Stamp a schema version and refuse clearly when newer | Medium | propose |
| Missing profile (active id gone) | `active_profile()` reads `None`; `ps run` says there is no profile (GUI path not checked here) | - | Owner test: delete the active profile in another library and reopen | Low | owner test |
| Read-only home / config dir | `write_config` error surfaces as "could not save libraries"; library writes fail per operation with their path | `access(2)` | Check at startup once and say which folder is read-only | Low | no |

## Runtime and tools

| Item | Now | System signal | Proposed | Risk | Owner |
|---|---|---|---|---|---|
| umu-run | Pinned release downloaded into `<library>/decanter/bin` on first launch, sha256 checked (`runtime/os/linux/umu.rs`) | - | - | - | optimal |
| Proton builds | umu downloads UMU-Proton on first launch; Steam's own found in `steam_compat_dir()` | - | First launch without network: confirm the error names the download | Low | owner test |
| Toolchain (PS builds) | Host tools when they cover the recipe, then the system's nix, then nix-portable downloaded on first need; a release bundle needs none (`crates/nix`) | - | - | - | optimal |
| MangoHud / GameMode missing | Toggles offered by capability; on but missing starts without, with a warning | Layer JSON / `gamemoded` + D-Bus | See settings table | - | optimal |
| Prefix placement | `<root>/compatdata/<account>/<game>`, one per game, created by umu | - | - | - | optimal |
| Shader caches | Per game inside the folder's cache location, made on launch; failing that, the drivers keep their own (warned) | - | - | - | optimal |
| Taskbar entries | Swept at startup, one symlink per entry in `~/.local/share/applications/decanter/` (`taskbar.rs`) | - | - | - | optimal |
| Self-update | Only an AppImage rewrites itself, into decanter's own copy when its folder is read-only (decision #1102) | - | - | - | optimal |
| GPU / driver | Nothing in decanter decides by vendor; psport chooses by Vulkan capability (decision #361) | - | - | - | optimal |
| Wayland / X11 | GPUI picks the platform; no decanter setting | `WAYLAND_DISPLAY`, `DISPLAY` | - | - | optimal |
| gamescope | Not used by decanter; only noted in MangoHud's handling | - | - | - | optimal |
| Keyring (GOG tokens, shadNet) | Secret Service over D-Bus (`credentials.rs`) | D-Bus Secret Service present | Unchecked here: a session without a Secret Service (gamescope session, bare WM) should make the first GOG login say so instead of a bare D-Bus error | Medium (where secrets live) | propose |

## First run paths

| Path | Now | Proposed | Owner |
|---|---|---|---|
| Clean home (no `libraries.toml`, no db) | First-run screen, language from the system, library at the default path | - | optimal; owner test |
| Moved library, `libraries.toml` updated, profile path spelled out | Profile reads the old place, "no game" (j261) | Commit 1 prevents it from now on; #1257 reports an already stale path | done / #1257 |
| Moved library, `libraries.toml` not updated | Unavailable screen naming the old path | A "find it" action (pick the new folder, relocate the pointer) | propose |
| Unplugged disk (active root) | Unavailable screen | - | optimal |
| Unplugged disk (profile's own root) | Empty library, and the PS section creates `psport/games` on the mount point | Report and don't create (#1257) | #1257 |
| No network | Library from the cache; installs and first Proton download fail with their error | Confirm by hand | owner test |
| First GOG login | Browser login, token to the keyring | Keyring check above | propose |
| First PS section open | Places the PS data in the profile's library, scans, creates `psport/games` | Don't create under a missing root (#1257) | #1257 |
