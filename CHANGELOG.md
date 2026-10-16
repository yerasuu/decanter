# Changelog

Grouped by area. Each version's technical record is in `tag-changelog/vX.Y.Z.md`.

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
