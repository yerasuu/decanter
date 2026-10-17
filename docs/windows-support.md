# Windows support

Recorded on 2026-10-12 as an option, with what it would take; started on
2026-10-16 (decision #439) with the non-critical parts.

## Phase 1: decanter builds and starts on Windows and macOS

The workspace compiles for Windows and macOS (CI checks
`x86_64-pc-windows-msvc`, `aarch64-apple-darwin` and `x86_64-apple-darwin`,
not yet blocking). Starting up works: first-run language, library, GOG
login, browsing the library and achievements. Platform differences are
`cfg(target_os)` functions where they arise:

- `libraries.toml` under the platform's config folder (`dirs`), the runtime
  lock in the local app data folder, free space through `fs4`, the keyring's
  native store (Credential Manager, Keychain).
- Linux-only, and said so in the UI ("not available on this system"):
  playing (Proton through `umu-run`), per-game Proton and prefixes, the
  tray, closing a game from a pad or shortcut, Steam export.
- Hidden or skipped: the PS section, desktop entries for the task bar,
  systemd scopes, and the Galaxy64 hook (not built, not deployed).

Next: native launching on Windows (Job Objects instead of scopes, the hook
DLL built natively), a tray, and macOS launching (no Proton there).

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
- The Galaxy64 hook stays undeployed: unlocks reach GOG through the game's
  own Galaxy integration, and decanter polls achievements after each
  session. Each GOG launch logs that the hook is absent.
- Not yet: a process that detaches from the one decanter started (a
  launcher re-executing the game) ends the session early; Job Objects
  would follow it. No DirectX DLL drop-ins yet.

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
| Tray via ksni | a cross-platform tray crate |
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
