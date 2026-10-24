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
  closing a game from a pad. The tray came to Windows and
  macOS in phase 2 (`tray-icon`), and the close-game shortcut to Windows
  (`global-hotkey`); the Steam export came to Windows later (see below).
- Hidden or skipped: desktop entries for the task bar,
  systemd scopes, and the Galaxy64 hook (not built, not deployed; see
  phase 2).

Next: native launching on Windows (Job Objects instead of scopes, the hook
DLL built natively) and macOS launching (no Proton there; see
[macos-support.md](macos-support.md)).

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
    icon, the task bar identity (Windows' AppUserModelID, through GPUI's
    `set_app_identity`, so without `unsafe` of decanter's own), whether the OS hit-tests the topbar's frame controls
    (`NATIVE_FRAME_CONTROLS`: Windows, with its caption dropped), exit
    signals (SIGTERM/SIGINT, Ctrl+C on Windows), detached spawns and the
    self-update's file calls. `nix`, `zbus` and `ksni` are Linux-only
    dependencies of the app; `tray-icon` is the tray on Windows and macOS
    (`platform/system_tray.rs`, shared by both), made on GPUI's main thread
    since its backends there need the thread running the event loop, and
    `global-hotkey` the close-game shortcut there (`platform/close_hotkey.rs`,
    Ctrl+Shift+F4 while a game runs, no pads), on the same thread for the
    same reason. The press and the cancellation post a GPUI system
    notification (`App::show_system_notification`, one tag,
    `decanter-close-game`, so the cancellation replaces the warning and the
    stop dismisses it); `CloseWatch::open` takes the `AsyncApp` for that,
    since the notification center is the main thread's. Windows delivers
    it because of the AppUserModelID above (`set_app_identity`). The text
    is `closing.notice.*` in the locales, `body_keyboard` for the
    keyboard-only body.
- **The launch backend** is named once per crate by its `build.rs`:
  `cfg(proton)` for code that only exists with Proton, `umu-run` and
  prefixes (`decanter-runtime`, `decanter-gog`, the app), `not(proton)` for
  the native backend, which Windows and macOS share (sessions per OS in
  `decanter_runtime::native`: a Job Object, a process group). It is set on
  Linux only. Which system's builds a game installs from is
  `decanter_core::platform::game_system` (Windows builds on Linux and
  Windows, macOS builds on macOS). `decanter-gog` also sets `cfg(galaxy_hook)` when its build
  script built `Galaxy64.dll` (Linux, x86_64 Windows).
- **Capabilities**: `decanter_core::capabilities` is the one source of what
  the UI offers and how. Each `Feature` is `Available`, `Testing` (shown
  with an "en pruebas" badge) or `Unavailable(Reason)`, the reason being
  `NotOnThisSystem`, `NeedsSetup(what)`, `ReportedBy(source, message)` or
  `NotPortedYet(source)` (a component not ported to this system yet).
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
- Views gate with `gate_or_reason` (the reason, muted), `gate_or` (their
  own text) and `reason` (a whole pane); `mark` badges what stays shown
  either way (the PS link) and `testing_badge` marks a menu item. No top
  bar section hides on any system (owner's rule, 2026-10-24): Library,
  Storage, Downloads and Settings never depended on a capability, and the
  PS section now shows on every system too, its page saying why where
  psport doesn't run (macOS: `NotPortedYet(Psport)`). The window closes into the tray
  only while `CloseToTray` is offered, so a missing tray host agrees with
  Ajustes.

| Feature | Linux | Windows | macOS |
|---|---|---|---|
| Play | available | available | testing (macOS builds only) |
| ExternalInstaller | available | available | no |
| GalaxyHook | available | available (x86_64) | no |
| Tray, CloseToTray | available | testing | testing |
| PadClose | available | testing (keyboard only) | no |
| SteamExport | available | testing | no |
| Proton, Prefixes, DesktopEntries, Scopes, SelfUpdate | available | no | no |
| PsSection | available | testing | no (page says psport isn't ported) |
| PsPlay | available | testing (psport.exe folder) | no |
| PsBuilds | available | no | no |
| MangoHud, GameMode | available | no | no |
| PsMenuBinding, PsOnline | once psport lists it | no | no |
| PsDownloads | available on x86_64 | no | no |

## The PS section on Windows (first slice)

Done on `feat/902-ps-windows-slice` (design note #901, decision #898).
decanter neither builds nor downloads psport here (`PsBuilds` and
`PsDownloads` are off: the Ports tab and the channel choice in Ajustes
say so, and mods and patches discovery and FSR 4.1.1's recording stop
at `prepare` with `ps.errors.builds_unavailable`); it runs a
psport build the user unpacked:

1. Unpack the `psport-windows-<commit>` package (psport's Windows CI
   artifact, or #899's test package) into any folder. It must hold
   `psport.exe` and `libpsport-gpu.dll` side by side, with the CLANG64
   DLLs the package carries; anything else in it (the report scripts) is
   ignored.
2. In decanter: PS › Ajustes › *Carpeta de psport.exe* › Elegir. The folder
   is taken only if it is an absolute path without `..` on a drive letter
   (no network share or device path, also once links, junctions and
   mapped drives are resolved with `fs::canonicalize`) holding both files
   as files of its own, not symbolic links
   (`decanter_ps::bbport::direct::Folder::try_new`), and kept as
   `psport_exe_dir` in the port settings (`psport/bbport/settings.json` in
   the library), beside the other PS folders. It is checked again at each
   launch.
3. Drop the game's dump in the library's PS games folder as on Linux and
   press Jugar.

How it starts: `Bbport::direct_command` gives `psport.exe` the same
`PS_*` environment `Bbport::launch_command` gives `run.sh` on Linux (one
shared `game_env`), plus what `run.sh` itself adds around the program
(`PS_CONFIG` pointing at the library's `psport.ini`, `PS_GPU_USER_DIR`
from the final `PS_USER_DIR`,
`PS_COPY_GPU_BUFFERS=1`, `PS_GPU_WRITE_TWINS=1`,
`PS_GPU_WRITE_TWINS_MAX=65536`, unless already set) and the arguments
`run.sh` builds from that environment (`--game`, `--system-modules`/
`--hle`/`--lle` with `PS_SYS_MODULES`, `--mods`/`--mods-config`/`--out`
unless mods are off, `--patches-dir`, `--patches-config`, `--app0`,
`--user`, `--timeout 0`; a path value starting with `-` goes as `.\-…`
so psport can't take it for an option). The process starts through
`decanter_runtime::native` in a Job Object session keyed by the dump
folder; its stdout and stderr feed the run log and the failures scanner
through `process::follow`, and Detener (or the close shortcut) ends it with
`native::stop`. AnyPS5 games aren't offered a launch (they need their
conversion toolchain).

Not done here yet: what `run.sh` computes in Bash (the scaled output's
render size, `PS_SAVE_LOG`'s own log files, MangoHud, the HOST quirk
environment, online play's helpers), asking `psport.exe --list-features`
for its table, downloads of the package, and the Vulkan validation layer
(only a system-wide one is found). Bloodborne's mods are linked with
symbolic links, which need Developer Mode on Windows.

Only a real Windows can test it (#440): picking the folder (a folder
without either file must be refused with its reason), Jugar on a dump
(game window, output in the Registro tab and the run's log file under the
library's `psport/logs`), Detener and the close shortcut ending
`psport.exe` and anything it started, a crash landing in Fallos, the Ports
tab and the channel choice showing their reason, and SmartScreen's prompt
on a downloaded `psport.exe`.

## Decanter: medium effort

Most of it is portable already: GPUI runs on Windows (Zed ships there) and
the code is Rust with portable crates. The Linux-specific parts have known
equivalents:

| Linux today | Windows |
|---|---|
| Proton / `umu-run` for GOG and external games | not needed: they run natively, which simplifies launching |
| Galaxy64 hook under Wine | works natively |
| One systemd scope per game | Job Objects |
| Per-game `.desktop` + symlink for the Plasma taskbar | shortcuts with a per-game AppUserModelID (decanter's own, `decanter`, is set through GPUI's `set_app_identity`) |
| `flock` runtime lock in `$XDG_RUNTIME_DIR` | `LockFileEx` |
| Keyring | Windows Credential Manager (the keyring crate supports it) |
| Tray via ksni | `tray-icon` (done) |
| `~/.config/decanter/libraries.toml` | `%APPDATA%\decanter\libraries.toml` |

The data-in-library layout carries over as it is. Decanter has almost no
`cfg(windows)` code today.

## Steam export on Windows

`decanter_core::steam` writes `shortcuts.vdf` and the grid art the same way
as on Linux (the shortcut's `Exe` is the game's own executable, which runs
natively here); only how Steam is found and driven differs, behind
`cfg(windows)`:

- `platform::steam_root_candidates`: `SteamPath` under
  `HKCU\Software\Valve\Steam` (read with `reg query`, since the registry
  has no safe binding among the dependencies; only an absolute path is
  taken), then `%ProgramFiles(x86)%\Steam` and `%ProgramFiles%\Steam`. A
  candidate counts once it has `userdata`, as before.
- `steam::process::state`: `tasklist /FI "IMAGENAME eq steam.exe"` instead
  of `/proc` and `steam.pipe`; a failure to run it is `Unknown`, which
  asks the user to confirm Steam is closed, as in a container.
- `shutdown` and `launch` run `steam -shutdown` / `steam` and fall back to
  `<root>\steam.exe` (`steam.sh` on Linux).
- The staged `shortcuts.vdf.pending` and the restart button work as on
  Linux: Steam rewrites its in-memory copy while it runs.

`SteamExport` is `Testing` on Windows. Nothing here was run on Windows.

## Closing a game from a pad on Windows: not done

There's no safe way to read a pad from the background with what the
workspace has. XInput (`XInputGetState`) is a C function, so calling it
needs `unsafe` (forbidden) or a binding crate not in the workspace;
`Windows.Gaming.Input` is safe through the `windows` crate but its
documentation doesn't say it reports input while another window has focus,
which a running game does. Kept as a question for the owner.

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
