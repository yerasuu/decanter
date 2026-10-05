# Decanter

GOG game launcher and achievement tracker for Linux. Runs Windows GOG
games through Proton/umu-run and shows their achievements, including
unlocks earned under Wine that GOG's own Galaxy client never sees.

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

## Achievements

Achievement unlocks tracked through the Galaxy64 hook are local-only:
decanter has no way to report them to GOG's own servers (no per-game
Galaxy credentials, no write API). Syncing to an external platform is a
possible future direction, not implemented yet.
