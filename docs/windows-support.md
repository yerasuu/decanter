# Windows support

Recorded on 2026-10-12 as an option, with what it would take; started on
2026-10-16 (decision #439) with the non-critical parts.

## Phase 1: decanter builds and starts on Windows and macOS

The workspace compiles for Windows and macOS (CI checks
`x86_64-pc-windows-msvc`, `aarch64-apple-darwin` and `x86_64-apple-darwin`,
not yet blocking). Starting up works: first-run language, library, GOG
login, browsing the library and achievements. Platform differences live in
the platform layer (see below):

- `libraries.toml` under the platform's config folder (`dirs`), the runtime
  lock in the local app data folder, free space through `fs4`, the keyring's
  native store (Credential Manager, Keychain).
- Linux-only, and said so in the UI ("not available on this system"):
  playing (Proton through `umu-run`), per-game Proton and prefixes,
  closing a game from a pad, Steam export. The tray came to Windows and
  macOS in phase 2 (`tray-icon`), and the close-game shortcut to Windows
  (`global-hotkey`).
- Hidden or skipped: the PS section, desktop entries for the task bar,
  systemd scopes, and the Galaxy64 hook (not built, not deployed; see
  phase 2).

Next: native launching on Windows (Job Objects instead of scopes, the hook
DLL built natively) and macOS launching (no Proton there).

## Phase 2: games launch natively on Windows

Games run as the Windows programs they are (2026-10-17): the launch
backend is split per OS (`decanter_runtime::native`, `app/src/launch/native.rs`
against the Proton ones), so on Windows a game's executable starts directly
from its own folder, with the same run log, playtime, running state and
stop. A stop asks the process tree to close (`taskkill /T`) and ends it
after a few seconds if it won't.

- Removed on Windows, not just disabled: the Compatibility settings tab and
  per-game tab, the external game form's installer, prefix import, library
  folder and UMU entry, and launcher detection inside prefixes. Manifest
  and settings fields for them (`Proton`, `UmuId`, `PrefixOf`,
  `default_proton`) stay readable and are ignored.
- Cloud save locations resolve against the user's own folders
  (`save_location::UserFolders::native`) instead of a prefix's
  `steamuser`.
- The Galaxy64 hook is built for the Windows target itself and deployed
  next to the game as on Linux (2026-10-17): unlocks are recorded locally
  without the Galaxy client, in `decanter/decanter-hook-<account>-<game>.jsonl`
  in the library, and decanter still polls achievements after each session.
- Not yet: a process that detaches from the one decanter started (a
  launcher re-executing the game) ends the session early; Job Objects
  would follow it. No DirectX DLL drop-ins yet.

## Platform layer and capabilities

Decided 2026-10-17 (decision #538), done on `refactor/535-platform-layer`.

- **System calls**: each crate that needs one has a `platform` module,
  `platform/mod.rs` plus `linux.rs`, `windows.rs` and `macos.rs`; `mod.rs` is
  the crate's only place that asks which OS it was built for, and the OS
  files export the same names. No `cfg(target_os)`, `unix` or `windows` is
  left outside them.
  - `decanter_core::platform`: the config, legacy data and default library
    folders, the runtime lock's folder, disk space (`statvfs` on Linux,
    `fs4` elsewhere), folder identity and the `/proc` check for the
    migration, Steam's roots, file and directory links, the executable mode,
    the system locale (`LC_ALL`, `LC_MESSAGES`, `LANG` on Linux; `sys-locale`
    on Windows and macOS, where a program started from the desktop has no
    such variable), and each OS's capability table (`support`).
  - `decanter_ps::platform`: process groups, SIGTERM and exit signals.
  - the app's: the close-game watch (pads, the Plasma shortcut and notices
    over D-Bus), the tray (ksni) and the tray host query, the X11 window
    icon, whether the OS hit-tests the topbar's frame controls
    (`NATIVE_FRAME_CONTROLS`: Windows, with its caption dropped), exit
    signals (SIGTERM/SIGINT, Ctrl+C on Windows), detached spawns and the
    self-update's file calls. `nix`, `zbus` and `ksni` are Linux-only
    dependencies of the app; `tray-icon` is the tray on Windows and macOS
    (`platform/system_tray.rs`, shared by both), made on GPUI's main thread
    since its backends there need the thread running the event loop, and
    `global-hotkey` the close-game shortcut there (`platform/close_hotkey.rs`,
    Ctrl+Shift+F4 while a game runs, no pads and no notice yet), on the same
    thread for the same reason.
- **The launch backend** is named once per crate by its `build.rs`:
  `cfg(proton)` for code that only exists with Proton, `umu-run` and
  prefixes (`decanter-runtime`, `decanter-gog`, the app), `not(proton)` for
  the native Windows backend. It is set everywhere but Windows: macOS still
  compiles the Proton code, having no backend of its own, and its table
  leaves Play off. `decanter-gog` also sets `cfg(galaxy_hook)` when its build
  script built `Galaxy64.dll` (Linux, x86_64 Windows).
- **Capabilities**: `decanter_core::capabilities` is the one source of what
  the UI offers and how. Each `Feature` is `Available`, `Testing` (shown
  with an "en pruebas" badge) or `Unavailable(Reason)`, the reason being
  `NotOnThisSystem`, `NeedsSetup(what)` or `ReportedBy(source, message)`.
  - `platform::support` is each OS's static table; code without a GPUI
    context (task bar entries, scopes, game tools, the hook, PS downloads,
    `decanter_runtime::available`) reads it directly.
  - `Capabilities::detect` takes away what the machine lacks: no systemd
    user manager (`Scopes`), no MangoHud Vulkan layer (`MangoHud`), no
    `gamemoded` with its D-Bus service (`GameMode`).
  - The app adds its own checks at startup (an `AppImage` it can rewrite
    for `SelfUpdate`; a StatusNotifier host for `Tray` and `CloseToTray`,
    asked over the session bus) and keeps the result as the `Caps` global,
    redrawing the windows when a check lands.
  - psport's own table (`psport --list-features`, its decision #540) is read
    each time the bbport port becomes ready (prepared, rebuilt, updated or
    installed from a bundle), checked at the edge (size, OS, plain ids,
    bounded reasons) and merged over decanter's: a feature a game needs
    (any area but `net`) that psport reports unavailable takes `PsPlay`
    away with psport's reason; an experimental one marks it as being
    tested. A newer report replaces the older one; a port from before
    `--list-features` reports nothing and decanter's table stands.
    `PsMenuBinding` (the Teclado and Mando rows for psport's `key.menu`
    and `pad.menu`) is the exception: off until a report lists
    `port_menu_binding`, since older builds ignore those lines. `PsOnline`
    (online play in Ajustes and Bloodborne's Online tab) is likewise off
    until a report lists `online_module`, and follows the `net` rows alone:
    an unavailable one takes it away with its reason, an experimental one
    marks it as being tested.
- Views gate with `Caps::gate` (hidden when unavailable), `gate_or_reason`
  (the reason, muted), `gate_or` (their own text) and `reason` (a whole
  pane); `testing_badge` marks a menu item. The window closes into the tray
  only while `CloseToTray` is offered, so a missing tray host agrees with
  Ajustes.

| Feature | Linux | Windows | macOS |
|---|---|---|---|
| Play, ExternalInstaller | available | available | no |
| GalaxyHook | available | available (x86_64) | no |
| Tray, CloseToTray | available | testing | testing |
| PadClose | available | testing (keyboard only) | no |
| Proton, Prefixes, DesktopEntries, Scopes, SelfUpdate, SteamExport | available | no | no |
| PsSection, PsPlay, MangoHud, GameMode | available | no | no |
| PsMenuBinding, PsOnline | once psport lists it | no | no |
| PsDownloads | available on x86_64 | no | no |

## Decanter: medium effort

Most of it is portable already: GPUI runs on Windows (Zed ships there) and
the code is Rust with portable crates. The Linux-specific parts have known
equivalents:

| Linux today | Windows |
|---|---|
| Proton / `umu-run` for GOG and external games | not needed: they run natively, which simplifies launching |
| Galaxy64 hook under Wine | works natively |
| One systemd scope per game | Job Objects |
| Per-game `.desktop` + symlink for the Plasma taskbar | shortcuts with a per-game AppUserModelID |
| `flock` runtime lock in `$XDG_RUNTIME_DIR` | `LockFileEx` |
| Keyring | Windows Credential Manager (the keyring crate supports it) |
| Tray via ksni | `tray-icon` (done) |
| `~/.config/decanter/libraries.toml` | `%APPDATA%\decanter\libraries.toml` |

The data-in-library layout and the Steam export carry over as they are.
Decanter has almost no `cfg(windows)` code today.

## psport: high effort, with prior art

In favour: the GPU side comes from shadPS4, which runs on Windows, and
there is a Windows port of bbport by Supermedo (the source of psport's
DLSS bridge).

The work is the runtime's platform layer (`src/`):

- **TLS**: PS4 code reads its TLS through `fs`; on Linux psport patches it
  to `gs`. On Windows x64 `gs` is the TEB and can't be repointed from user
  mode. The main technical question; Supermedo's port solves it somehow.
- **Memory**: fixed-address mappings, direct memory and the guest memory
  shared with Vulkan (dma-buf/memfd on Linux) become `VirtualAlloc`, file
  mappings and Vulkan's Win32 external memory.
- **Signals**: write traps and the Unity GC's thread suspension become
  vectored exception handlers and `SuspendThread`/`GetThreadContext`.
- **Threads, files, build**: pthreads to Win32; the Nix toolchain to CMake
  with clang-cl or MinGW and vcpkg.

17 files already have `_WIN32` guards, mostly stubs ("Windows backend not
implemented").

## Ongoing cost

Two platforms to test; today all game testing (headless gamescope,
`tools/regress.sh`) is Linux-only.

## Suggested order, if pursued

1. Meanwhile: keep new psport code free of Linux-isms outside a clear
   platform layer.
2. Decanter on Windows for GOG and external games (best value for effort).
3. psport on Windows after studying how Supermedo's port handles TLS and
   memory (a read-only feasibility study first).
