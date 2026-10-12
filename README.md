# Decanter

Game launcher and achievement tracker for Linux. Runs Windows GOG games,
and any other Windows game you add to it, through Proton/umu-run, and
shows GOG achievements, including unlocks earned under Wine that GOG's
own Galaxy client never sees.

It also runs the user's own PS4 dumps natively on Linux, through
decanter's fork of bbport ([psport](https://github.com/yerasuu/psport)):
a compatibility layer in the manner of Wine, not an emulator: the game's
x86-64 code runs as it is and the port provides its system libraries.

## Layout

- `crates/core` — local state: cached library metadata, achievements,
  accounts/profiles, credentials.
- `crates/gog` — GOG client: auth, library listing, installs, cloud
  saves, achievement polling.
- `crates/runtime` — launches games through Proton via `umu-run`
  (fetched and cached by decanter, not resolved off `PATH`).
- `crates/hook` — `Galaxy64.dll` proxy that captures achievement
  unlocks under Wine and logs them for decanter to pick up. Cross-compiled
  as a Windows cdylib, excluded from the main workspace.
- `crates/ps` — PS4/PS5 dumps: reading them, building and updating the
  ports (psport, AnyPS5) and their toolchain, running a game and recording
  its log and what the port left unimplemented. No GPUI.
- `app` — the app itself (`decanter`), built with GPUI. Also exposes
  a headless CLI (`decanter <subcommand>`) for library listing,
  installs, launches, and achievement polling without the GUI.

## Building

### With Nix

`shell.nix` pins every dependency (Vulkan/Wayland/X11 libraries GPUI
loads at run time, mingw cross toolchain for `crates/hook`,
`umu-launcher`, TLS env vars) — nothing else to install:

```
nix-shell --run 'cargo run'              # dev
nix-shell --run 'cargo build --release'  # binary
scripts/appimage                         # target/appimage/Decanter-x86_64.AppImage
```

The AppImage carries the nix glibc the binary was built against, so it
runs on hosts with an older one too; see the script for how.

The binary links against nix's glibc, which can be newer than the host's
(SteamOS's is): if it refuses to start with ``GLIBC_2.xx' not found``,
start it through nix's own loader from inside the shell:

```
nix-shell --run '$(cc -print-file-name=ld-linux-x86-64.so.2) target/debug/decanter'
```

### Without Nix

Same commands, no `nix-shell --run` wrapper, once these are on the
system instead:

- Rust.
- `pkg-config`, `dbus`, and what GPUI loads at run time: `vulkan-loader`,
  `wayland`, `libxkbcommon`, the X11/xcb libraries, `fontconfig`,
  `freetype` — check `shell.nix`'s `runtimeLibs` for the exact list.
- `umu-launcher` on `PATH` — decanter spawns `umu-run` from it.
- A `x86_64-pc-windows-gnu` target (`rustup target add
  x86_64-pc-windows-gnu`) plus a mingw-w64 cross gcc and its
  `libpthread.a`, only if rebuilding `crates/hook`'s `Galaxy64.dll`.

## Languages

The app is in Spanish and English. By default it follows the system
locale: the first of `LC_ALL`, `LC_MESSAGES` and `LANG` that is set picks
Spanish when it starts with `es`, and English otherwise. Ajustes >
General > Idioma (Settings > General > Language) overrides it, and goes
back to the system's with "El del sistema" (System default). The change
applies right away, except for the tray menu and text field
placeholders, which switch on the next start, and messages already
shown, which keep the old language until they're shown again. The
headless CLI and everything decanter writes (its log, the PS run logs
and `failures.jsonl`) aren't translated: they stay in English whatever
the language, and the UI words their data when it shows it.

The texts live in `app/locales/`, one YAML file per area, each key with
its `es` and `en` value.

## Achievements

Achievement unlocks tracked through the Galaxy64 hook are local-only:
decanter has no way to report them to GOG's own servers (no per-game
Galaxy credentials, no write API). Syncing to an external platform is a
possible future direction, not implemented yet.

## PS4 and PS5 games

The PS section lists the dumps in its games folder and runs PS4 titles
through psport and PS5 titles through AnyPS5. Decanter clones each port,
keeps it up to date and rebuilds it when its checkout moves. It builds
with the host's tools when they're there and falls back to its own Nix
toolchain (nix-portable if the system has no Nix) when the host can't
configure the port; after the next update of that port it tries the host
again. Each game runs in its own process group and
can be stopped from its row, its cover or its context menu.

Bloodborne is supported as upstream bbport ships it. Every other PS4
title is experimental; six of the seven tested reach gameplay with
sound. Titles that require PS VR (`require_ps_vr` in their `param.sfo`)
are shown as unsupported and can't be started. The user's own dump of
the console's system modules can be set in Ajustes, but no title needs
it today: the port implements those libraries itself, and the dump is
only for comparing with them.

Every job's whole output goes to a log under the PS folder, headed by
the game, the port commit, the command and its environment, keeping the
last 30 per game. Fallos records, once per run, what the port left
unimplemented, starting with the imports the game actually called.

Where each title stands and what the fork adds: `docs/ps4-ports.md`.
What the titles import and what the port still lacks:
`docs/ps4-import-matrix.md`.

## Where decanter keeps its data

Everything decanter stores lives inside the profile's library root
(`<root>`), so a library can be moved or copied with its profiles:

- `<root>/decanter/decanter.db`: profiles, accounts, game cache,
  achievements, playtime, collections and settings of the profiles whose
  library this is.
- `<root>/decanter/art/` (art cache), `<root>/decanter/bin/` (`umu-run`),
  `<root>/decanter/{desktop,icons}/` (per-game desktop entries and icons).
- `<root>/decanter/logs/decanter-YYYY-MM-DD.log`: decanter's own log, the
  lines it prints to stderr plus its debug events, in English, one file
  per UTC day, two weeks kept.
- `<root>/psport/`: everything PS (games, ports, toolchain, settings,
  mods, logs).
- Game files, manifests, prefixes and install staging, as before.

Outside it only what can't move: `~/.config/decanter/libraries.toml`
(the known libraries and the active one), one symlink per game in
`~/.local/share/applications/decanter/` (Plasma only reads its own
folders), the keyring (GOG tokens, SteamGridDB key), Steam's own files,
umu's shared runtime and a lock in `$XDG_RUNTIME_DIR`. `--library <root>`
or `DECANTER_LIBRARY` pick a library for one run. A missing library is
reported, never created.

On its first start this version moves the old `~/.local/share/decanter`
data into the library (copying, checking and only then switching) and
leaves the old folder untouched as a backup; Ajustes > Almacenamiento
offers to remove it afterwards.
