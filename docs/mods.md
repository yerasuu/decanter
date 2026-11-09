# Third-party mods

What is built of [third-party-mods-design.md](third-party-mods-design.md): phases 1 to 4 for GOG, Amazon Games and Epic Games
games: loose folders and zip or 7z archives, DLL overrides and load order, update checks and the uninstall dialog. The code is `crates/mods` (data, import, archives, play tree) and `app/src/mods.rs` (launch);
the Mods tab is `app/src/views/library/properties/mods/`.

## Using it

- Propiedades → Mods → *Añadir carpeta de mod...*: pick a folder laid out like the game's own folder (its files go
  to the same relative paths). The folder is copied in and never changed; the mod is turned on for the account
  that added it.
- *Añadir archivo comprimido...* (or an archive dropped on the tab): a zip or 7z. It is hashed and every entry
  checked first, with a progress line and *Cancelar*; then a preview shows its name (editable), file count and
  size, where its files go (the game's folder or one of its folders, preselected from the archive's top level; a
  single wrapping folder that matches nothing of the game is left out), the game's files it replaces, and whether
  it carries an installer (FOMOD or setup program), which is never run. Nothing is written before *Añadir*; the
  same archive added twice points at the mod it already is. RAR is refused with a line saying so.
- The checkbox turns a mod on or off for this account. A mod turned on goes last and wins a file over the ones
  already on; ↑ and ↓ move an enabled mod in the load order (`profiles/<account>.toml` `mods`), where the lower one
  wins. Changes apply at the next launch; nothing can be changed while the game runs.
- DLL proxies: a mod file named after a system DLL Wine has a builtin for (`dinput8`, `version`, `winmm`, `dxgi`,
  `d3d9`, `d3d11`, `dsound`, `xinput1_3`, `winhttp`) in a folder of the game that holds a `.exe` gets the override
  `n,b` at import, stored in its `mod.toml` (`[[dll_overrides]]`). The Mods tab lists each with a selector for its
  mode (`n,b`, `b,n`, `n`, `b`, not loaded) and says which enabled mod wins a DLL two of them ask for (the later
  one). Under Proton the enabled mods' overrides are appended to the launch's `WINEDLLOVERRIDES`, and the run's log
  header names them (`mod_dll_overrides`); Compatibilidad shows them read-only. On a Windows host they are ignored.
- Each enabled mod lists the game's files it replaces and the files it wins or loses against other mods.
- *Quitar* asks first, with the mod's size and how many accounts use it, then deletes it for every account.
- *Comprobar archivos* hashes a mod's files against its `mod.toml` and lists those that changed (a game that rewrote
  a config through its link writes the mod's copy).

## Updates

- When an account's set is applied, `profiles/<account>.toml` `[base]` records the build and, per game path a mod
  replaces or adds (folded), the install file's `sha256:` or `absent`. The comparison runs again only when the
  installed build differs from `[base].build_id`: at launch, when the Mods tab opens, and right after an update in
  Actualizaciones, which then says how many mods need attention.
- A changed file (or a path only a mod had that the update now ships) puts every enabled mod touching it in
  `[attention]`: it is left out of the play tree (the run log's `mods_left_out` says so) and the Mods tab lists the
  files with *Seguir usándolo* (records the new files) and *Desactivarlo*. Nothing is decided for the user.

## Uninstall

- The confirmation says how much the game wrote into this account's play tree, which goes with the prefix
  (`play/<account>/` and its record; the set stays for a reinstall).
- When no other account still has the game installed, a box *also remove N mods (size)*, off by default, deletes the
  whole `mods/<store>/<game>/` folder; otherwise the mods are kept and the dialog says so.

## Thunderstore

*Añadir desde Thunderstore…* opens a browser in the tab (`crates/thunderstore`, `mods/thunderstore/` in the app;
[mod-platforms-analysis.md](mod-platforms-analysis.md) section 2). No account; nothing is asked of Thunderstore until
the user acts, requests are paced to one a second and name decanter in the User-Agent, HTTPS only, redirects only to
`*.thunderstore.io`.

- Community: the first time, the game's title is matched against `/api/experimental/community/` (cached a week in
  `<root>/decanter/thunderstore/communities.json`); the user confirms the match or picks one, with the layout, and the
  choice is kept in `mods/<store>/<game>/thunderstore.toml`. *Cambiar* picks again.
- Packages: `/c/<community>/api/v1/package/`, streamed to `<root>/decanter/thunderstore/listings/<community>.json`
  (6 hours, at most 1 GiB) and searched locally: name, team, summary, downloads, rating, last update, icon. The detail
  lists versions and the latest one's dependencies. A package or version that does not parse is skipped and counted
  in the log; an answer that does not parse at all is a retryable error.
- *Instalar* plans the latest version and every `Team-Name-1.2.3` it needs, recursively (10 levels, 100 packages,
  cycle-safe; the higher version wins; a dependency already installed from Thunderstore is not downloaded again and
  is listed with the version asked for; one the community lacks refuses the install). The zips download into
  `mods/<store>/<game>/incoming/thunderstore-<pid>-<n>/` with progress and *Cancelar*, each matched against the size
  the listing gives, then each is read by the archive import above, `icon.png`, `README.md`, `manifest.json` and
  `CHANGELOG.md` at its root left out. Nothing is added before *Añadir*; dependencies go first in the load order.
- Layout, per game: *BepInEx* follows r2modman's convention as the analysis reads it (not verified against
  r2modman): a package with `BepInEx/` goes to the game's root, `plugins/`, `patchers/`, `config/` and `monomod/` go
  under `BepInEx/`, a package with none of them into `BepInEx/plugins/<Team-Name>/`; one mixing those folders with
  other files is placed as any archive and the preview says so. *Como cualquier archivo* places every package as any
  archive. Making BepInEx load under Proton (`winhttp` and its doorstop) is the DLL proxy rule above, nothing more.
- `mod.toml` gets `[origin]` (`platform = "thunderstore"`, `package`, `version`, `host`). Such a mod shows it with
  *Buscar actualizaciones*, which compares it with the latest version in the community's listing.

## Where things are

Under the library root that holds the install (`<root>/common/<InstallDir>`):

```
<root>/decanter/mods/<store>/<game id>/    <store>: gog, amazon or epic
    <mod uuid>/mod.toml, files/    one mod, shared by every account that has the game
    profiles/<account>.toml        the account's enabled mods in load order, and the disabled ones
    play/<account>/                the play tree the game starts from
    play/<account>.toml            every link decanter placed in it, and its target
    incoming/                      imports and removals in progress
```

- The play tree is real folders and one relative symbolic link per file, to the install's file or the winning
  mod's (`../../…/common/<InstallDir>/…`, `../../<mod uuid>/files/…`), so moving the library keeps it valid. On a
  Windows host the links are hard links, since a symbolic link needs a privilege there.
- Anything the game writes in its folder (configs, saves next to the executable) lands in the play tree as a real
  file and is never replaced. A cloud save folder inside the install is read from the play tree instead.
- The game starts from the play tree once an account has turned a mod on, and keeps starting from it with every mod
  off, since it may hold the game's files. With no mod ever on there is no play tree and nothing changes.
- Verify, repair and update read and write only `common/<InstallDir>`: a mod never writes into the install. This holds
  for all three stores, whose installs are all `common/<InstallDir>` with a manifest per account; an Epic DLC
  installed into the game's folder is mirrored at the next launch like any file of the install.
- When the library root is outside the folders the Steam Linux Runtime shares by default, the launch adds it to
  `PRESSURE_VESSEL_FILESYSTEMS_RW`.

## Rules an import keeps

- Only regular files and folders; a link, device or name that is not UTF-8 refuses the whole import (in an archive:
  zip symlink modes and Unix types, 7z reparse points, Unix types and anti-items).
- Archive names: `\` read as `/`, then no absolute path, drive letter, `..` or `.`; any bad entry refuses the
  archive. Encrypted archives are refused, and so are 7z methods decanter does not build (bzip2, zstd, brotli, lz4)
  and LZMA or PPMd dictionaries larger than 1 536 MiB.
- Archives: at most 200 000 entries, and each entry (each solid block in 7z) larger than 1 MiB at most 1 000 times
  its compressed size. Sizes are enforced again while extracting, since a header can lie; files are created
  `0644` and folders `0755` in a fresh `incoming/` folder, and a cancel or failure removes it.
- Paths: relative, at most 1 024 bytes and 64 components, no `.`, `..`, `\`, `:` or control characters.
- Two names that are the same file for a Windows game (differing only in case) refuse the import.
- `Galaxy64.dll` and `Galaxy64.orig.dll` are kept for decanter's Galaxy hook and refuse the import.
- At most 200 000 files and 64 GiB per mod, and 1 GiB left free on the library's disk.
- Files are copied into `incoming/<pid>-<n>/`, created new (never through an existing file), hashed (SHA-256),
  then renamed into place; a failure removes what was copied.

Every `mod.toml`, set file and play tree record is checked again when read; a mod file that is not a plain file
inside its mod's `files/` fails the launch instead of being linked.

## Not yet

RAR, external games, renaming a mod once added, a target folder for a loose folder, and Nexus Mods or mod.io.
