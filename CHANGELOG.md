# Changelog

Grouped by area. Each version's technical record is in `tag-changelog/vX.Y.Z.md`.

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
