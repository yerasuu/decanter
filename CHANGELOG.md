# Changelog

Grouped by area. Each version's technical record is in `tag-changelog/vX.Y.Z.md`.

## v0.4.0

### Breaking

- Only for the error-report server, new in this version's development: its single admin key is gone, so a config with `admin_key`, `admin_key_file` or `admin_key_env` no longer loads, and admins log in with a key from `decanter-reports user add <name> --role admin`. Error reports use schema 2 (the message is a template with typed values, never free text); a client of the first schema is refused. Nothing a release up to v0.3.4 wrote is affected.

### gog

- Downloads pause, resume and survive a crash: a journal in the library's `downloading/` folder keeps what was written, a pause on closing, a lost network, a full disk or an unplugged library, and carries on by itself at the next start (switch in Ajustes). A new build keeps the chunks it shares with the old one.
- One download at a time, in an order you set: Downloads is grouped like Steam, with a reorderable queue, Pausar, Reanudar and Cancelar, also for updates, DLC and repairs. DLC ticked with a game wait for it.
- Every chunk is checked against GOG's sizes and MD5s; cloud saves are written atomically.
- Pick a game's language at install and in Propiedades: only that language's files are downloaded, and the files of unused languages can be removed.
- Redistributables a build names are installed once per prefix before the game starts, and the program to launch can be picked among the build's own.
- GOG's fan-made mods are listed, with the ones you own marked; a DLC without files of its own installs; builds on GOG's `gcdn.co` CDN (Cyberpunk 2077) install.
- GOG access runs on decanter's own async client; the old third-party client is gone.

### library

- Third-party mods for GOG games: add a folder of loose files, enable it, see conflicts; the game starts from a mirror of its install, so verify, repair and updates leave the install untouched.
- Add a game by dropping its executable: name and icon are read from the file, and Linux programs run natively.
- A profile's library path that names its own library follows the folder when it is moved.

### ps

- Every game has its own settings: launch settings, port ini, online and module switches, mods and patches, and layouts, copied from your defaults the first time and backed up first. A game's Resumen copies from another game, exports, imports and resets; PS Ajustes edits the defaults.
- `decanter ps run`, `prepare`, `settings` and `report` work without the window.
- Error reports: preview and save the exact report in Fallos, and send it from Reportar errores after confirming; private names are kept out and nothing leaves without your confirmation.
- Online: per-game TLS and clear-text login switches; shadNet secrets reach psport by a private file or stdin, not by environment variable.
- Per-game frame generation (Off or FSR) and Local player; the game menu's frame generation choice is remembered; the compatibility state of each dump (playable, experimental, broken, unsupported) shows in the list.
- Add a dump with Add dump or by dropping its folder, copying or moving it; a dump loose in a games folder is put in its own folder.
- Bloodborne's built-in patches ship with decanter; the Online tab shows only for games that use an online library.
- New Rendimiento and Importaciones tabs per game: performance lines apart from the log, and the imports psport does not cover.
- DETENER works from the moment a launch starts and ends the game at once.

### runtime

- decanter fetches and checks its own umu-run and GE-Proton 11-6 (no Steam needed), with a card in Compatibility to remove it.
- Raven, an experimental runtime for Windows programs, can be switched on in Ajustes, built from its pinned commit (or its branch's newest) after confirmation, and used in its own section.
- Deleting builds, packages, the toolchain, GE-Proton and Raven asks first.

### app

- Secrets (GOG sessions, shadNet passwords, the SteamGridDB key) live in one store: the system keyring, or an encrypted passphrase file when there is none or it stays locked. Old plaintext copies are moved; Ajustes lists and forgets saved credentials.
- Logs, reports, run logs and the database are private to your user; reports, copied logs and debug-folder copies are scrubbed of secrets, addresses and names.
- The install dialog always opens, also from the game's menu.
- An AppImage in a read-only folder updates itself into its own copy and hands over to it.
- Quitting stops every psport job; the library is ready as soon as it is listed; the database no longer reports "database is locked" from repeated migrations.
- A hint in Registro when winewayland refuses a screen mode.

### i18n

- Interface in English, Spanish, German, French, Italian, Brazilian Portuguese, Russian, Simplified Chinese, Japanese and Korean, picked in Ajustes; all but English and Spanish are machine translations and are marked so. Missing text falls back to English.

### Windows (testing)

- The PS section plays psport's Windows package, downloaded from its releases, or a build of psport's branch made here with a pinned toolchain, with no Nix or bash; PS Ports, builds and downloads are no longer marked as tested.
- `decanter-cli.exe` runs the command line from a terminal; `decanter.exe` opens no console and carries its icon.
- Online play is not offered on Windows.

### error-report server

- `decanter-reports`, a separate server in `crates/reports-server`, receives reports, re-validates them and publishes a clean public version; maintainers get named accounts with viewer and admin roles, an audit log, and a private log viewer with search and comparison.

## v0.3.4

### macOS (testing)

- macOS plays the native macOS builds of GOG games, with no compatibility layer: installed from GOG's `osx` builds, started from their app bundle, stopped as a process group. A game with no macOS build says so instead of offering Instalar.
- Cloud saves look in Documents and Application Support on macOS.
- Nothing has run on a Mac yet.

### Windows (testing)

- Closing a game with Ctrl+Shift+F4 shows a notification with the countdown and its cancellation.
- Exporting to Steam works on Windows: Steam's folder comes from the registry or its default folders, and a running client is detected.
- The PS section plays with a `psport.exe` folder picked in its Ajustes, started directly with no Nix or bash; ports, builds and channels say why they are off.
- Nothing has run on a real Windows yet.

### library

- GOG's product record says whether a game has a macOS build; the answer is cached in a new `games_cache` column that older databases gain on opening.

### app

- The PS section shows on every system; where psport doesn't run, its page says why (macOS: not supported yet).

## v0.3.3

### Windows (testing)

- Playing, external installers and the GOG achievements hook are no longer marked as being tested on Windows; the tray, close-to-tray and the close-game shortcut are.
- decanter draws its own window frame on Windows, keeping the system's drag, maximize and snap layouts.
- A game's session lasts until every process it started has exited, so a launcher that starts the game and quits no longer ends it; stopping closes them all.
- The tray works on Windows and macOS. Ctrl+Shift+F4 closes the running game on Windows.
- The UI language follows the system's on Windows and macOS; the task bar groups decanter under its own identity.
- Tag releases attach `decanter.exe` when CI built it.

### app

- Debug mode in Ajustes > General gathers decanter's and psport's logs in one folder (by default inside the library), a subfolder per computer, written whole before a sync tool can take them; also switchable per PS game.
- Each library folder chooses where its games' shader caches go; PS games can live in extra folders and move between them; external games and PS dumps show their size, measured again only when they change.
- Notices and errors change language with the UI, and a language is now only a folder of files.
- Issues and comments that ask to run code from an untrusted source are hidden or closed automatically.

### desktop

- Steam is detected through its `steam.pipe` when `/proc` hides it (Flatpak, containers), so shortcuts are no longer written under a running client.

### gog

- A session GOG closes keeps the library offline on its cache, with no error; decanter offers to renew it (silent refresh, then browser login), also from Ajustes > Cuentas.
- A token refreshed by a call that then fails is no longer lost.

### ps

- Online play from the PS section: shadNet server, WebAPI, community server, NPID and UPnP, plus Bloodborne's BBSC with its local server; the password and token stay in the keyring. Offered when the port lists its online module.
- New psport settings: Bloom switch, saved runs kept, camera cuts, image traps, DLSS, controller motion, toy pad, anisotropic filtering, Intel tone mapping fix, system fonts folder and keyboard stick behaviour (only when the port supports it). Shown defaults match psport's.
- Upscalers and fixes the machine can't run are hidden, following the port's own report.
- Testing takes the port's CI builds of each commit and builds locally only what is missing.
- The Vulkan validation layer is fetched on demand when the setting is on; without it the game starts and says why.
- The effects query has a 30 s limit, so a hung port no longer leaves the graphics tab waiting.
- Saving settings drops the 0.3 memory model keys once the port ignores them.

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
