# Changelog

Grouped by area. Each version's technical record is in `tag-changelog/vX.Y.Z.md`.

## v0.3.2

### Windows (testing)

- decanter builds and starts on Windows and macOS. Features each system can't offer are hidden, and the ones still being tested carry an "en pruebas" badge. No Windows build is published yet; CI builds one.
- On Windows, games launch natively, without Proton, umu or prefixes, with the same run log, playtime and stop. Save paths use the user's own folders.
- When a game that imports DirectX 7 crashes on Windows, the run log and the Registro tab suggest a DirectDraw wrapper.
- The external-game form runs `.exe` and `.msi` installers on Windows, elevated through UAC, then opens the executable picker in Program Files.
- GOG achievements are recorded on Windows too, through the same Galaxy proxy beside the game, without GOG Galaxy.

### app

- The AppImage installs decanter's new releases itself once their minisign signature checks out. Ports, `umu-run`, nix-portable and UMU-Proton are checked for updates when asked.
- Rounder theme: larger corner radii, floating menus, dropdowns and dialogs more rounded, pill progress bars.
- Achievement dates follow the UI language and local time zone; the GamesDB rating shows as 75/100.
- Sidebar rows that can't be played now are dimmed instead of marked with a green dot, which stays for running games.

### runtime

- MangoHud can be turned on for all games or per game, with its logs kept in the library.
- Games can start through GameMode, globally or per game.
- Both are only offered when they are installed, and apply to PS games too.

### ps

- Release ports run from psport's published bundle, checked against its SHA-256, so a host without nix no longer needs nix-portable for them.
- A port build that fails after an update goes back to the last commit that built; automatic updates skip the failing commit until retried.
- decanter reads what the port can do from `psport --list-features` and only offers what it supports.
- The game's aspect ratio follows the display (4:3, 16:10, 16:9, 21:9 or 32:9) unless set by hand.
- A per-game switch measures each run: the Registro shows average FPS, 1 % low, frame times, shader-compile stalls, an FPS chart and the change from the previous run.
- The gamepad is edited on a drawing of the pad with rows of button and action, Juego/Port tabs and changes applied together; shipped layouts are read-only, and an own configuration can be restored to the one it came from.
- The keyboard screen shows what each key does in the game.
- A key or pad button can open the port's menu, on ports that support it.
- The PS section follows the app's theme.

## v0.3.1

### toolchain

- The release AppImage is published again: the build script's download no longer breaks on the CI runner. v0.3.0 was never published as a GitHub Release, so this release carries its changes.
- An AppImage for aarch64 (`Decanter-aarch64.AppImage`) is published next to the x86_64 one.
- .deb, .rpm and pacman packages for x86_64 and aarch64, made from the same build as the AppImage: they install it under `/opt/decanter` with a `decanter` command, a desktop entry and an icon. Updates come from new packages; self-update stays AppImage-only.
- CI checks the workspace with Rust stable, beta and its minimum version (1.95), and starts every release build on Ubuntu 24.04 and 26.04 and in Ubuntu, Debian, Fedora and Arch Linux containers before publishing it.

## v0.3.0

### Breaking

- decanter's data (database, art, `umu-run`, logs) moves from `~/.local/share/decanter` into each library's `decanter/` folder, found through `~/.config/decanter/libraries.toml`. The first start copies it over, checks it and leaves the old folder as a backup; older builds keep using the old folder and don't see later changes.
- GOG downloads in progress are staged in the library (`<root>/downloading/`) instead of the old data folder.

### app

- Ajustes is a section of its own, holding profiles and the library switcher; the first start asks for the language.
- Switching profile switches to its library; refused while a game runs or installs.
- decanter keeps a daily log in English inside the library, two weeks kept, with secrets left out.
- Running games are marked in the Biblioteca and PS sidebars.

### cli

- `--library <root>` (or `DECANTER_LIBRARY`) runs against a given library.

### library

- A library can span several folders; installs pick one, games move between them, unplugged disks are reported.
- Games that HoYoPlay or GRYPHLINK installed inside an external game's prefix can be added as their own entries.
- Each Proton game's output is logged and shown in its Registro tab.
- Moving a library carries its profiles, desktop entries and symlinks along.

### runtime

- Each game runs in its own systemd scope named after it.
- Holding Select + Start for two seconds, or Ctrl+Shift+F4 on Plasma, closes the running game after a cancellable notice; can be turned off in Ajustes.

### desktop

- Each game has a desktop entry with its title and icon, so the task bar shows the game instead of decanter.

### i18n

- The UI is in Spanish (Spain) and English, following the system locale or the language chosen in Ajustes, switched live.

### ps

- The PS4 port is psport; a per-library channel picks stable releases fetched prebuilt from psport's Nix cache, or Testing built from `develop`.
- Ports follow psport's renamed variables, files and listing flags, and older releases still run.
- Mouse sensitivity, mouse look and inverted Y settings; the in-game menu follows decanter's language.
- Runs remember when the port turned frame overlap off for a title, until forgotten.
- Every PS4 title other than Bloodborne runs as experimental; PS VR titles are marked unsupported; the system module dump is optional.
- A port the host can't build is built with nix; moved checkouts rebuild.
- Each job's whole output is logged; Fallos lists what the port left unimplemented, with expected statuses apart from failing calls.
- Right-click menu on PS games' rows and covers.

### toolchain

- CI checks format and clippy, and attaches the AppImage and the version's notes to `v*` releases.
- decanter is licensed GPL-3.0-or-later.

## v0.2.0

### app

- New PS section in the sidebar, between Biblioteca and Almacenamiento, with Juegos, Ports, Fallos and Ajustes tabs.
- External games are added from Biblioteca; the Ajustes "Externos" tab is gone.

### library

- External games: any executable, or one installed by its own setup program, runs with its own prefix and Proton pin.
- Art for external games from SteamGridDB (with a personal API key) and Steam, stored in the game's manifest.
- Import a game from another tool's Wine prefix (Lutris, Bottles, Heroic, Proton), moved or copied with progress.
- Moving a library keeps symlinks as links.
- Deleting an external game forgets its favorites, playtime and collections.

### runtime

- External games launch as `umu-default`, or under a UMU database entry picked by name, so protonfixes applies.
- Games no longer inherit the dev shell's nix Vulkan drivers.

### ps

- Runs PS4 and PS5 dumps through bbport (Bloodborne 1.09, CUSA03173) and AnyPS5, built with the host's tools, the system's nix or a bundled nix-portable.
- Port checkouts follow upstream automatically; rewritten history or local commits wait for the user, with local diffs backed up.
- PS data and games live in the profile's library under `psport/`.
- Per-game controls: editable gamepad and keyboard layouts, bbport settings, mods, patches and the FSR 4.1.1 build.
- Bloodborne's button icons follow the gamepad layout and pad style.
- Port failures are kept per profile with surrounding output and can be copied as reports.
