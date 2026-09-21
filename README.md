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
- `ui` — the app itself (`decanter`), built with Dioxus. Also exposes
  a headless CLI (`decanter <subcommand>`) for library listing,
  installs, launches, and achievement polling without the GUI.

## Building

### With Nix

`shell.nix` pins every dependency (webkitgtk, gtk3, mingw cross toolchain
for `crates/hook`, `umu-launcher`, TLS/GL env vars) — nothing else to
install:

```
nix-shell --run 'dx serve'                                    # dev
nix-shell --run 'cargo build --release'                       # binary
nix-shell --run 'unset SOURCE_DATE_EPOCH && dx bundle --package decanter --platform linux --package-types appimage --release'  # AppImage
```

`SOURCE_DATE_EPOCH` (set by Nix for build reproducibility) makes
`mksquashfs` reject appimagetool's own timestamp flag — unset it just
for this step, only needed under Nix.

### Without Nix

Same commands, no `nix-shell --run` wrapper, once these are on the
system instead:

```
dx serve                                    # dev
cargo build --release                       # binary
dx bundle --package decanter --platform linux --package-types appimage --release  # AppImage
```

- Rust + the [Dioxus CLI](https://dioxuslabs.com/learn/0.6/CLI/) (`dx`).
- `pkg-config`, `webkitgtk` 4.1, `gtk3`, `xdotool`, `openssl` — the
  desktop build links against these; check `shell.nix`'s `runtimeLibs`
  for the exact list if something's missing at link or run time.
- `umu-launcher` on `PATH` — decanter spawns `umu-run` from it.
- A `x86_64-pc-windows-gnu` target (`rustup target add
  x86_64-pc-windows-gnu`) plus a mingw-w64 cross gcc and its
  `libpthread.a`, only if rebuilding `crates/hook`'s `Galaxy64.dll`.

## Achievements

Achievement unlocks tracked through the Galaxy64 hook are local-only:
decanter has no way to report them to GOG's own servers (no per-game
Galaxy credentials, no write API). Syncing to an external platform is a
possible future direction, not implemented yet.
