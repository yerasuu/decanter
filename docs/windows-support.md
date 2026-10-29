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

## No console window: `decanter.exe` and `decanter-cli.exe`

Release builds of `decanter.exe` use the GUI subsystem (`windows_subsystem` in `app/src/main.rs`),
so starting it from Explorer opens no console; debug builds keep one for the log. A GUI-subsystem
program has no console of its own, and attaching to the parent's needs `unsafe` or a dependency, so
the CLI subcommands are run through `decanter-cli.exe` (`app/src/cli_shim.rs`): a console program
that starts the sibling `decanter.exe` with the same arguments and the terminal's standard handles,
and exits with its status. It ships next to `decanter.exe` and never needs updating, since the CLI
lives only in `decanter`.

Cargo can't gate a `[[bin]]` by target, so the bin has `required-features = ["windows-cli"]` and the
Windows build passes `--features windows-cli` (CI's Windows build does). Every process decanter
starts on Windows is created with `CREATE_NO_WINDOW` through `without_console`, except games.

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
- **The launch kind** is chosen per launch, not per build: every system
  compiles the same `app/src/launch`, one module per kind with the same
  functions (`umu` for Proton in a prefix, `linux_native`, `native`), and
  `Kind::of` picks one from the capability table (`Feature::Proton`, on
  Linux only) and the game's file. Views, installers and the CLI ask
  `launch::in_prefixes`. `decanter_runtime` has one interface on every
  system, its per-OS parts under `os/` (sessions in `os/native.rs`: a Job
  Object, a process group). Which system's builds a game installs from is
  `decanter_core::platform::game_system` (Windows builds on Linux and
  Windows, macOS builds on macOS). `decanter-gog` also sets `cfg(galaxy_hook)` when its build
  script built `Galaxy64.dll` (Linux, x86_64 Windows).
- **Capabilities**: `decanter_core::capabilities` is the one source of what
  the UI offers and how. Each `Feature` is `Available`, `Testing` (shown
  with an "en pruebas" badge) or `Unavailable(Reason)`, the reason being
  `NotOnThisSystem`, `NeedsSetup(what)`, `ReportedBy(source, message)`,
  `NotPortedYet(source)` (a component not ported to this system yet) or
  `DownloadsInstead(source)` (not compiled on this system yet: its published
  release is downloaded ready-made instead).
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
| PsPlay | available | testing (Windows package or psport.exe folder) | no |
| PsBuilds | available | testing (the branch built with the pinned toolchain and MinGit) | no |
| MangoHud, GameMode | available | no | no |
| PsMenuBinding, PsOnline | once psport lists it | no | no |
| PsDownloads | available on x86_64 | testing (the release's Windows package) | no |

## The PS section on Windows (first slice)

Done on `feat/902-ps-windows-slice` (design note #901, decision #898).
decanter doesn't run the Linux build pipeline here (no checkouts or nix:
mods and patches discovery and FSR 4.1.1's recording stop at `prepare`
with `ps.errors.builds_unavailable`); since #927 it builds psport's branch
its own way (`PsBuilds`, see below). Since #917 it downloads
the release's Windows package (next section); a psport build the user
unpacked still works as the manual alternative:

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
3. Add the game's dump with «Añadir volcado…» (or drop its folder on the
   window) as on Linux, and press Jugar.

How it starts: `Bbport::direct_command` gives `psport.exe` the same
`PS_*` environment `Bbport::launch_command` gives `run.sh` on Linux (one
shared `game_env`), plus what `run.sh` itself adds around the program
(`PS_CONFIG` pointing at the library's `psport.ini`, `PS_GPU_USER_DIR`
from the final `PS_USER_DIR`, unless already set) and the arguments
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
for its table, and the Vulkan validation layer (only a system-wide one is
found). Bloodborne's mods are linked with
symbolic links, which need Developer Mode on Windows.

Only a real Windows can test it (#440): picking the folder (a folder
without either file must be refused with its reason), Jugar on a dump
(game window, output in the Registro tab and the run's log file under the
library's `psport/logs`), Detener and the close shortcut ending
`psport.exe` and anything it started, a crash landing in Fallos, the Ports
tab and the channel choice showing their reason, and SmartScreen's prompt
on a downloaded `psport.exe`.

## psport's Windows package (download, check, update)

Done on `feat/917-windows-psport-download` (decisions #918, #919, #921;
the owner's rule of 2026-10-24: versions are published only by tag).
Only the Release channel exists here: psport's CI attaches
`psport-windows-vX.Y.Z-x86_64.zip` and `psport-windows-vX.Y.Z-x86_64.zip.sha256`
(`<sha256>  <name>`, as the Linux bundle's) to each tag's release (#920).
Testing builds are compiled locally with an encapsulated toolchain (see
the next section); nothing per commit is downloaded.

`decanter_ps::os::windows::package` (compiled everywhere, offered where `PsDownloads`
is):

- **Listing**: `offers` asks the [releases API](https://docs.github.com/en/rest/releases/releases#list-releases)
  of `Port::github_repo` anonymously (`yerasuu/psport`, 30 per page, the
  `X-GitHub-Api-Version: 2022-11-28` header, a 8 MiB cap, 403 with
  `x-ratelimit-remaining: 0` or 429 read as the rate limit). A release
  counts when its tag is `vX.Y.Z` as Linux accepts it
  (`prebuilt::valid_tag`, no pre-release suffix) without leading zeros,
  it is neither a draft nor marked as a pre-release, and it
  lists both assets by their exact names, the zip between 1 byte and
  `bundle::MAX_BYTES` (512 MiB). Newest first by version number.
- **Download**: the `.sha256` comes from `<release downloads>/<tag>/` (the
  same URL and `DECANTER_PS_BUNDLES` override as the Linux bundles) and
  must be one `<64 lowercase hex>  <asset>` line; the zip is streamed to
  `psport-windows/.<tag>.<pid>.zip` with the size the release lists checked
  against `Content-Length`, while it arrives and at the end, then the
  SHA-256 (`bundle::download`, shared with Linux, which passes no size).
  Redirects are followed only over HTTPS to `github.com`,
  `objects.githubusercontent.com` and `release-assets.githubusercontent.com`
  ([where release assets are served from](https://docs.github.com/en/actions/reference/runners/self-hosted-runners)),
  at most five; Linux keeps reqwest's default policy. An install holds a
  lock file, `.<tag>.<pid>.lock`, and starts by removing what other
  processes' installs left unless their lock is still held.
- **Names before extracting**: `tar -t -f` and `tar -t -v -f` list the
  archive (the same entries in the same order); every kind must be `-` or
  `d` (no links or special files), every name one top folder plus
  components of ASCII letters, digits and `._+-` (so no `..`, absolute
  paths, drive letters, `\`, `:` or streams), no trailing dot, none of
  Windows' device names (`CON`, `PRN`, `AUX`, `NUL`, `COM0`–`COM9`,
  `LPT0`–`LPT9`, with any extension;
  [naming files](https://learn.microsoft.com/en-us/windows/win32/fileio/naming-a-file)),
  no name twice regardless of case, at most 4096 entries; the size column of
  `tar -tv` gives each file's size, at most 256 MiB each and 1 GiB together.
- **Extraction**: Windows' own `%SystemRoot%\System32\tar.exe` (libarchive's
  bsdtar, which reads zip: [tar](https://learn.microsoft.com/en-us/windows-server/administration/windows-commands/tar),
  [bsdtar](https://man.freebsd.org/cgi/man.cgi?query=bsdtar)), by full path
  so Git's GNU tar on `PATH` is never taken, without a console window
  (`CREATE_NO_WINDOW`), into `psport-windows/.<tag>.<pid>/`. What lands
  there must be the listed files exactly, of their listed sizes, plain
  files and folders (a link or junction is refused), and: `.exe` only `psport.exe`,
  `psport-gpu-capabilities.exe` and `psport-windows-probe.exe`; `.dll`
  only `libpsport-gpu.dll` and those `BUILD-INFO.txt` lists under
  `DLLs included (N):` (N of them, each present); both `.exe` and `.dll`
  beside `psport.exe`; `psport-probe.bat` and `psport-probe.ps1` the only
  scripts; everything else `.txt`, `.md`, `.json` or `.ini`. The DLL list
  is trusted from the release itself: what vouches for those DLLs is
  psport's CI, which builds and lists them, and the release's SHA-256,
  not an independent list in decanter. Then the folder is renamed to `<library>\psport\psport-windows\<tag>\` and
  `<tag>.json` beside it records the tag, asset, SHA-256, size and
  download time; only a folder with a record that reads back is used,
  and one is never replaced: an install finding it there keeps it.
  Nothing from the package runs during any of this.
- Linux and macOS run the same checks with `bsdtar` and `/usr/bin/tar`
  (both libarchive), though only Windows installs packages.

In the app (`state/ps/package.rs`, `views/ps/package.rs`): PS › Ports shows
a *psport para Windows* card: the version
in use, the releases with their date and size and the versions in the
library, to download, use (`psport_windows_version` in the port settings)
or delete (never the one in use, nor any while a game runs). From the
first time the section opens, and every 30 minutes or with *Buscar
actualizaciones*, it lists the releases and downloads and uses the newest
unless the user chose another version (`psport_windows_pinned`) or a
psport.exe folder, both read again from the settings once the download
ends. A release whose automatic install failed is left alone by the
periodic check until a newer one appears or the user checks by hand. Jugar runs the chosen psport.exe folder when there is
one, else the version in use, downloading the newest release first when
the library has none.

To try a package before CI publishes one: serve a folder laid out as
`<tag>/<asset>` (the zip and its `.sha256`) and point
`DECANTER_PS_BUNDLES` at it (`http://` or `file://`); listing still asks
GitHub, so the tag must also be a published release.

Only a real Windows can test it, once a release carries the package:
the Ports card listing it, the download with its progress, the folder and
record in the library, Jugar from it, *Usar* and *Borrar* on two versions,
a failed check (offline) showing its message, and SmartScreen on the
downloaded `psport.exe`.

## psport's branch built on Windows (Testing)

Done on `feat/927-windows-build-psport-branch` (decisions #924 and #928,
psport #926). `decanter_ps::os::windows::winbuild` builds the branch Linux's Testing
channel follows on this machine, with a toolchain decanter downloads and
pins itself, and leaves the result beside the packages. `PsBuilds` is
offered (Testing) on Windows; git is decanter's pinned MinGit, never the
user's, so nothing has to be installed first (decision #1107). Where
`psport.exe` runs directly the Linux pipeline (checkouts, nix, the update
loop, `prepare`) stays off whatever `PsBuilds` says.

**What runs.** Only, from the downloads, MinGit's `cmd\git.exe` (with
`GIT_CONFIG_NOSYSTEM=1`, so MinGit's own `etc\gitconfig` is not read, and
`http.sslBackend=schannel`, so certificates are checked against Windows'
store; the user's own git configuration still applies), the pinned `cmake.exe` and
`ninja.exe` and what they start: llvm-mingw's clang and lld, and
`pkgconf.exe`, `glslang.exe` and `cmd.exe /C` for CMake's custom commands.
Nothing built is run; Jugar runs it once the user chooses it.

**Pins** (`crates/ps/src/os/windows/winbuild/pins.rs`), checked against the official
release pages on 2026-10-24:

| Piece | Asset | SHA-256 | Where the hash comes from |
|---|---|---|---|
| llvm-mingw 20261020 | [llvm-mingw-20261020-ucrt-x86_64.zip](https://github.com/mstorsjo/llvm-mingw/releases/tag/20261020), 190,612,575 B | `fd5462b7…74cd` | the asset's digest in GitHub's releases API (no checksum file upstream) |
| CMake 4.4.4 | [cmake-4.4.4-windows-x86_64.zip](https://github.com/Kitware/CMake/releases/tag/v4.4.4), 54,400,562 B | `bace36e9…c536` | `cmake-4.4.4-SHA-256.txt` of the release, signed by Kitware |
| Ninja 1.13.2 | [ninja-win.zip](https://github.com/ninja-build/ninja/releases/tag/v1.13.2), 291,570 B | `07fc8261…dc65` | the asset's digest in GitHub's releases API |
| psport toolchain 1 | [psport-toolchain-win32-1-x86_64.zip](https://github.com/yerasuu/psport/releases/tag/toolchain-win32-1), 317,092,630 B | `4f9ee32b…87d0` | the release's `.zip.sha256`, matching the asset's digest in GitHub's API and the downloaded zip |
| MinGit 2.56.0.4 | [MinGit-2.56.0.4-64-bit.zip](https://github.com/git-for-windows/git/releases/tag/v2.56.0.windows.4), 39,846,926 bytes | `e3bf78351fe777fba47ddee5950082f8a98bb0f02604fdc7ba3ee2cb68ffa877` (the release's own digest, matching `sha256sum` of the downloaded zip) | the SHA-256 the release notes list for the asset, matching `sha256sum` of the downloaded zip |

The published manifest (`psport-toolchain-win32-1-x86_64.json`, schema 1,
psport commit `f3c6bb653`) names the three tools by the SHA-256, sizes and
folders above. `pins::TOOLCHAIN` holds the package's number, SHA-256 and
size in one place; a value there that is no SHA-256 stops every build with
*this decanter pins no psport toolchain package yet* before anything is
downloaded. To pin a new `toolchain-win32-N`:

1. Read the release's assets (`gh release view toolchain-win32-N -R
   yerasuu/psport --json assets`): the zip's size and digest, and the
   `.zip.sha256`, which must agree; download the zip and compare its
   `sha256sum` too.
2. Check that the `.json` asset (the zip's `manifest.json`) names the three
   tools above by the same SHA-256 and folders; if psport moved a tool,
   update `pins::tools()` from that tool's official release page first.
3. List the zip (`bsdtar -tvf`) against the caps and names below: entries,
   unpacked bytes, the largest file, and any character beyond them.
4. Set `pins::TOOLCHAIN` to `{ number: N, sha256: "<64 hex>", size: <bytes> }`,
   resizing the package's caps if step 3 asks for it (never loosening the
   names), and build once on Windows.

MinGit is pinned apart, in `pins::MINGIT` (`tag`, `version` as the asset
names it, `sha256`, `size`); its SHA-256 still says `unpinned`, which stops
every check and build with `NotPinned`. To pin a release:

1. On the release page
   (`https://github.com/git-for-windows/git/releases/tag/v<git>.windows.<n>`),
   read the SHA-256 the notes list for `MinGit-<version>-64-bit.zip` and the
   asset's size (`gh release view v<git>.windows.<n> -R
   git-for-windows/git --json assets`, whose `digest` must agree).
2. Download the zip and compare `sha256sum MinGit-<version>-64-bit.zip`.
3. List it (`bsdtar -tvf`): entries, unpacked bytes, the largest file and
   any character beyond `Names::Toolchain`; set `mingit()`'s caps from it
   and write the measures beside them and in the table below.
4. Set `pins::MINGIT` and remove `MINGIT_SHA256`'s placeholder, then check
   the branch and build once on Windows without Git for Windows on `PATH`.

**Toolchain cache** (`winbuild::toolchain`): `<library>\psport\toolchain\win32\<folder>\`
per piece, `<folder>.json` recording the SHA-256 the zip was checked
against and `<folder>.files` the SHA-256 of every file extracted from it
(`sha256sum`'s format). Each zip is downloaded with `bundle::download`
(size and SHA-256 while it arrives and at the end, redirects only over
HTTPS to GitHub's release hosts, the 512 MiB cap) to `.<folder>.<pid>.zip`,
extracted with `archive::unpack` (below) into `.<folder>.<pid>\`, its
required files checked (clang, clang++, ld.lld, llvm-ar, llvm-ranlib,
llvm-windres, llvm-dlltool; `cmake.exe`; `ninja.exe`; `cmd\git.exe`; `manifest.json`,
`psport-toolchain.cmake`, `pkgconf.exe`, `glslang.exe`), hashed, then
renamed into place and recorded. One install of a piece at a time, across
processes (`.<folder>.lock`): the second finds it installed and keeps it,
and a folder with a valid record is never replaced; holding the lock, an
install removes what other processes' interrupted installs of that piece
left. A piece counts while its record names the current pin and its
required files and list are there; a new pin downloads it again.

Before each build every file of the five pieces is hashed again against its
list: a file changed, missing, added or turned into a link stops the build
with *the toolchain's <file> changed after it was installed and checked*,
and *Borrar la toolchain* lets the next build download it again. The DLLs a
build copies from `clang64\bin` are read once, hashed against the list and
written from those bytes, since the compile ran in between. Then the
package's `manifest.json` must be schema 1, name
`psport-toolchain-win32-N-x86_64` with the pinned number, `psport-toolchain.cmake`
and `clang64`, and list `llvm-mingw`, `cmake` and `ninja` with the pinned
SHA-256 and folders (other fields, such as `msys2_database`, are not read).
About 560 MB to download the first time, 2 GB on disk, plus MinGit; hashing the 2 GB
takes seconds per build.

**Archive checks** (`crates/ps/src/archive.rs`, shared with the package of
the previous section, which keeps its own limits and names unchanged):
`tar -t` and `tar -tv` first; only files and folders; one top folder of the
expected name (Ninja's zip has none: `Top::Flat`); every component ASCII
letters, digits and `._+-`, not `.`/`..`, no trailing dot, no device name;
no name twice regardless of case; then extraction into staging and a walk
that must find exactly the listed files at their listed sizes, no links or
junctions. Per archive, sized from what each holds:

| Archive | Measured | Entries cap | Unpacked cap | Per-file cap | Names |
|---|---|---|---|---|---|
| llvm-mingw | 9,299 entries, 749,515,686 B, largest 77.7 MB | 12,000 | 1 GiB | 128 MiB | plain |
| CMake | 8,979 entries, 143,310,580 B, largest 33.9 MB | 12,000 | 256 MiB | 64 MiB | toolchain |
| Ninja | 1 file, 603,648 B | 4 | 4 MiB | 4 MiB | plain |
| MinGit | 378 entries (375 files), 96,237,338 bytes unpacked, largest file 9,413,944 bytes, longest path 63 (no top folder: `Top::Flat`) | 1,000 | 256 MiB | 32 MiB | toolchain |
| toolchain 1 | 54,558 entries (51,863 files), 1,070,375,859 B, largest 33.1 MB, longest path 159 | 80,000 | 1.5 GiB | 256 MiB | toolchain |

*Toolchain* names (`Names::Toolchain`) add only inner spaces (never first
or last; CMake's HTML help, `Unix Makefiles.html`) and `@` (gettext's
locale folders in the package, `sr@latin`, 15 of them); a device name
followed by spaces is still refused. The package stays `Names::Plain`.

**Sources** (`winbuild::git`): the remote is `Port::Bbport.upstream()`
(`https://github.com/yerasuu/psport.git`, Linux's), and the only submodule
fetched is `gpu/third_party/imgui` from `https://github.com/ocornut/imgui.git`,
both written in the code; `.gitmodules` from the fetched tree is never read.
*Consultar la rama* runs `git ls-remote --symref --end-of-options <remote> HEAD refs/heads/*`:
the branch is `develop`, or the default branch without one, as on Linux,
held to `updater::valid_branch`, and its commit must be 40 lowercase hex.
The build then `git init`s an empty folder and runs
`git fetch --depth=1 --no-tags --no-recurse-submodules --end-of-options <remote> <commit>`,
`git checkout --detach --end-of-options <commit>`, and reads `HEAD` back; the
imgui commit is the one `git ls-tree --end-of-options <commit> gpu/third_party/imgui`
records (mode 160000, 40 hex), fetched the same way. Every git call:

- starts with `env_clear()` and only `SystemRoot`, `SystemDrive`, `PATH`,
  `USERPROFILE`, `HOME`, `APPDATA`, `ProgramData`, `TEMP`, `TMP`, `TMPDIR`
  and the proxy variables of the caller, so no `GIT_DIR`, `GIT_CONFIG_*` or
  other `GIT_*` reaches it; then `LC_ALL=C`, `GIT_TERMINAL_PROMPT=0`,
  `GCM_INTERACTIVE=never`, `GIT_LFS_SKIP_SMUDGE=1`, and
  `GIT_CEILING_DIRECTORIES` so it never looks for a repository above the
  work folder;
- runs in that folder (or the checkout) as its working folder and `-C`;
- passes `-c protocol.allow=never -c protocol.https.allow=always
  -c http.sslVerify=true -c credential.helper= -c core.hooksPath=NUL
  -c core.symlinks=false -c core.protectNTFS=true -c core.longpaths=true
  -c core.fsmonitor=false -c transfer.fsckObjects=true -c submodule.recurse=false
  -c filter.lfs.smudge= -c filter.lfs.process= -c filter.lfs.required=false`;
- has a timeout (15 minutes over the network, 5 locally).

The user's own and the system's git configuration still apply where these
options don't override them (a proxy, an `insteadOf`): it is the user's
machine.

**Confirmation.** *Compilar* asks once per commit, showing remote, branch
and commit and that building runs the branch's CMake scripts with the
user's rights and may reach the network; *Confirmar y compilar* records
it in `psport-windows\confirmed.json` (the last 200), and `winbuild::build`
refuses any commit not recorded there, whoever calls it.

**Paths and locks.** Ninja on Windows refuses a path past 260 characters
and the deepest file of a psport checkout sits 190 characters below its
root, so the work folder is `<library>\pw\<first 8 hex>\` (beside the
`psport` folder rather than in it, every character counts): the checkout in
`s`, the build in `b`, temporary files in `t`. A build whose checkout path
would pass 68 characters stops at once with the path, its length and the
advice to move the library (`winbuild::MAX_CHECKOUT_PATH`); the default
library (`C:\Users\<name>\AppData\Local\decanter\steamapps`) fits with a user
name of up to 12 characters. The lock is per work folder
(`<library>\pw\<8 hex>.lock`), so two commits sharing those 8 digits never
build at once: the second gets *being built by another decanter*. After
taking it, a build returns the finished build another process made
meanwhile, and removes the staging folders (`psport-windows\.<commit>.<pid>`)
of builds no process runs any more. The work folder and the staging
folder go when the build ends, also when *Detener* drops it (on a thread
of their own); the whole output stays in `<library>\pw\<commit>.log`.

**Output.** Each line of the tools' output is cut at 4 KiB (the rest of the
line is read and dropped, never held), at most 1024 lines wait for the log
(more are dropped and counted, and the log says how many), and the log file
stops at 64 MiB; the card shows the last 12 lines.

**The build** (`winbuild::run`, `platform::confine`, `platform::build_env`):
`cmake.exe -S <s> -B <b> -G Ninja -DCMAKE_MAKE_PROGRAM=<ninja.exe>
-DCMAKE_TOOLCHAIN_FILE=<package>/psport-toolchain.cmake -DPS_LLVM_MINGW=<llvm-mingw>
-DCMAKE_BUILD_TYPE=RelWithDebInfo -DPS_LTO=OFF -DPS_COMMIT=<12 hex>`, then
`ninja.exe -C <b> -j <min(cores, 16)>`, every path absolute with `/`, no shell.
Each tool starts with `env_clear()` and only `SystemRoot`, `windir`,
`SystemDrive`, `ComSpec`, `PATH=%SystemRoot%\System32`, `PATHEXT`, `TEMP` and
`TMP` (the work folder's `t`), no console window, and joins a Job Object
right after it starts:

- `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`: the job is closed when the tool
  exits, when the step passes its bounds or the user presses *Detener*, and
  when decanter itself ends, and everything in it ends with it. Output still
  open 10 seconds after the tool exited is no longer waited for.
- No `JOB_OBJECT_LIMIT_BREAKAWAY_OK` nor `..._SILENT_BREAKAWAY_OK`, so no
  process can leave it.
- Asked as well, best effort since Windows may refuse them to a standard
  user (the log then says so): `BELOW_NORMAL_PRIORITY_CLASS` and a
  per-process working set of at most 6 GiB, past which Windows trims pages
  rather than kills.
- Bounds checked by decanter every 500 ms: configure 15 minutes and 64
  processes, compile 90 minutes and 128 processes (psport's CI compiles in
  about 7 minutes on 4 vCPU; 16 Ninja jobs plus their `cmd.exe` wrappers
  stay well under 128). Past either, the job is closed.

**Result** (`winbuild::assemble`): the three programs and
`gpu\libpsport-gpu.dll` from the build, and the DLLs of the toolchain
package's `clang64\bin` they import, as the closure of their PE import and
delay-import tables read by decanter itself (`winbuild::pe`, bounds-checked,
at most 96 sections, no tool run), `vulkan-1.dll` left to the driver as
psport's own bundle does, copied into `psport-windows\.<commit>.<pid>\` with
a `BUILD-INFO.txt` that lists them (`DLLs included (N):`, the package's
format). The folder must pass `package::check_programs` (only those
programs and DLLs, texts otherwise) and `Folder::try_new`, so it is
accepted exactly as a downloaded package; then it is renamed to
`psport-windows\<commit>\` and `<commit>.json` records the remote, branch,
toolchain and its SHA-256, and the DLLs. Package listings skip these, since
a commit is no `vX.Y.Z` tag.

**Accepted risks and what is not enforced.**

- A confirmed branch's CMake scripts run with the user's rights: they can
  reach the network (Windows has no unprivileged way to stop that short of
  an AppContainer, out of scope by decision #928; the build needs none, as
  FetchContent is disconnected and its sources come in the package) and
  anything the user can. The confirmation per commit of the one
  allow-listed remote is what stands against it (#924).
- The toolchain's hashes catch a build that changed the toolchain before
  the next build uses it, not one that also rewrites decanter's own records
  or anything else of the user's: the records live in the library with the
  same rights.
- win32job 2.0.3 (decision #804) exposes no `ActiveProcessLimit`, job memory
  limit or `JOBOBJECT_BASIC_UI_RESTRICTIONS`
  ([Job Objects](https://learn.microsoft.com/en-us/windows/win32/procthread/job-objects),
  [UI restrictions](https://learn.microsoft.com/en-us/windows/win32/api/winnt/ns-winnt-jobobject_basic_ui_restrictions)),
  and calling `SetInformationJobObject` directly needs `unsafe`, which the
  workspace forbids: the process count is decanter's polling (a listing
  past 1024 processes reads as too many), memory is bounded by Ninja's job
  count and the working-set cap, and there are no UI restrictions.
- As with a game's session, the tool runs for a moment before it joins its
  job, and a process it starts in that window would escape (a suspended
  start needs `unsafe`).

**In the app** (`state/ps/branch.rs`, `views/ps/branch.rs`): PS › Ports
shows, under the package card, *Rama de psport, compilada en este PC*: the
toolchain's state, *Consultar la rama* with the newest commit, *Compilar*
with its confirmation, the stage (downloads with their progress, unpacking,
sources, configuring, compiling, assembling), *Detener*, the last lines of
the output, and the builds in the library to *Usar* (`psport_windows_version`
holds the commit, pinned so the release updates leave it) or *Borrar*, and
*Borrar la toolchain*. Nothing is deleted while a game or a build runs or a
choice is being saved, and never the build in use. Jugar runs the chosen
folder, else the chosen build, else the package as before. PS › Ajustes
says the choice is made in Ports instead of showing the Linux channels.

Checked on Linux only (2026-10-24), with a throwaway harness against a
local `python3 -m http.server` and local git repositories mapped onto the
allow-listed URLs by a test `~/.gitconfig`:

- the four real pieces installed through the real pins, the 317 MB
  toolchain zip included (its 51,863 files hashed and verified again);
- refused archives: a wrong SHA-256, a wrong size, a pin that is no SHA-256,
  `..`, an absolute name, a link, `CON.txt`, `aux.h`, a stream, a case
  duplicate, a space in a plain archive, a leading space, a trailing dot,
  two top folders, a file beside the folder, a backslash, a non-ASCII name,
  too many entries, a file or a total too large, a missing required file;
  a new pin downloading again and another process's leftovers swept;
- a toolchain file changed or added after install, a DLL replaced in
  `clang64\bin` before the copy, and `ninja.exe` changed between two builds,
  all refused as changed;
- a folder assembled from llvm-mingw's real PE files with the DLL closure
  `objdump -p` gives, and an extra `.exe`, an unlisted DLL, a link and a
  missing listed DLL refused;
- git with `GIT_DIR`, `GIT_CONFIG_GLOBAL`, `GIT_CONFIG_COUNT`/`KEY`/`VALUE`
  and `GIT_CONFIG_PARAMETERS` set in decanter's environment, all ignored:
  `develop` and its commit, the checkout with imgui at its recorded commit
  and a link checked out as a file; an unknown commit, a branch named
  `weird;name`, a tree without the imgui gitlink and a `file://` remote
  without explicit permission refused;
- a step past its time ended with its whole process tree, a failing tool, a
  clean environment, a 10 MB line cut at 4 KiB, 300,000 lines with the
  overflow counted;
- an unconfirmed commit and a long library path refused before anything is
  downloaded; a finished build asked for twice at once returned twice; a
  second build of the same work folder refused as busy; the whole pipeline
  up to CMake (which can't run on Linux) with a dead build's staging swept,
  the work folder removed and the log kept.

Only Windows can check the rest: `tar.exe` on the real zips (names with
spaces and `@` included), the Job Object (its limits accepted for a
standard user, kill on close, the process count), MinGit with
the clean environment, `core.hooksPath=NUL` and fsck, CMake and Ninja with
the clean environment, the build itself, the card, and Jugar on a built
folder.

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
