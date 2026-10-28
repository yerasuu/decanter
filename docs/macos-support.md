# macOS support

decanter builds and starts on macOS since phase 1 (see
[windows-support.md](windows-support.md), which also holds the platform
layer and the capability table for every system). On macOS only the
native macOS builds of GOG games are installed and played, with no
compatibility layer: a game GOG publishes for Windows alone is listed, and
shows why it can't be installed or played (decision #836).

Designed in note #891 and built in task #865. Nothing here has run on a
real Mac yet: Play is marked as being tested.

## How it works

- **Launch kind**: the capability table offers no Proton here, so a game
  takes the `native` launch kind, as on Windows (`app/src/launch/native.rs`
  over `decanter_runtime`'s `os/native.rs` and `os/macos.rs`). A game's program starts directly as the
  leader of a process group of its own
  ([`process_group(0)`](https://docs.rs/tokio/1.53.2/tokio/process/struct.Command.html#method.process_group)).
  The session lasts while anything in the group runs (signal 0 to the group,
  [POSIX `kill`](https://pubs.opengroup.org/onlinepubs/9799919799/functions/kill.html)),
  and a stop sends the group SIGTERM a few times, then SIGKILL. A process
  that leaves the group (`setsid`) leaves the session too: macOS has no
  Job Objects or cgroups to follow it.
- **Builds**: the content system's builds are asked per system,
  `os/windows` on Linux and Windows and `os/osx` on macOS
  (`decanter_core::platform::game_system`, `GameSystem`). GOG documents
  none of `content-system.gog.com` ([its docs](https://docs.gog.com/) cover
  the publisher side); the endpoint and its `osx` value come from the
  community's [gogapidocs](https://gogapidocs.readthedocs.io/en/latest/galaxy.html).
- **Which games have one**: GOG's product batch (`api.gog.com/products`,
  already read by the library's second pass) carries
  `content_system_compatibility.osx` ([gogapidocs](https://gogapidocs.readthedocs.io/en/latest/galaxy.html),
  unofficial). It fills `Game::has_macos_build`, cached in the
  `games_cache.has_macos_build` column (added in place, so an older
  decanter still opens the database). Unknown counts as having one: the
  install then answers.
- **Install**: GOG asks publishers to ship one app bundle and to make it
  the installation directory ([recommendations](https://docs.gog.com/bc-recommendations-for-mac/),
  [depots for an app bundle](https://docs.gog.com/bc-macos-depot/)), so
  the install lands as `<library>/common/<installDirectory>/Contents/...`.
  The launch executable is `Contents/MacOS/<CFBundleExecutable>` from
  `Contents/Info.plist` ([Apple](https://developer.apple.com/documentation/bundleresources/information-property-list/cfbundleexecutable)),
  read with the `plist` crate (XML or binary, MIT, decision #892); a
  `<Name>.app` folder inside the install directory is found too. Files the
  depot flags `executable` (undocumented; as
  [gogdl](https://github.com/Heroic-Games-Launcher/heroic-gogdl/blob/main/gogdl/dl/managers/task_executor.py)
  reads it) get mode 755, and the bundle's program always does.
- **Cloud saves**: templates resolve against remote-config's `MacOS`
  section, with `DOCUMENTS` and `APPLICATION_SUPPORT`
  (`~/Library/Application Support`) and `INSTALL`; Galaxy's default storage
  is `~/Library/Application Support/GOG.com/Galaxy/Applications/<client>/Storage`
  ([Storage](https://docs.gog.com/sdk-storage/)). GOG documents no
  template names; `MacOS` and `APPLICATION_SUPPORT` are what Heroic reads
  ([save_sync.ts](https://github.com/Heroic-Games-Launcher/HeroicGamesLauncher/blob/main/src/backend/save_sync.ts)).

## Not there yet

- The achievement hook (`Galaxy64.dll` is a Windows DLL), the close-game
  shortcut and pads, external games (their form picks `.exe` files) and
  the PS section.
- Launching through LaunchServices (`open`): the program is run directly,
  which keeps its PID and output but skips what `open` would add.

## To check on a Mac

1. A game with a macOS build shows Instalar; one without shows "Este juego
   no tiene versión para macOS".
2. Installing it: `os/osx` returns builds, the files land in an app bundle
   and the manifest's launch executable is `Contents/MacOS/<name>`.
3. First run: Gatekeeper and unsigned or unnotarized binaries; the game
   starts, the Registro tab follows its output, playtime counts, and
   Detener closes it (SIGTERM, then SIGKILL).
4. Intel against Apple Silicon (Rosetta): GOG lists no architecture.
5. Cloud saves find the game's folders and sync.
6. Verify and repair keep the program's executable mode.
