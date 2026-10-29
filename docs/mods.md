# Third-party mods

What is built of [third-party-mods-design.md](third-party-mods-design.md): phase 1, loose folders for GOG games.
The code is `crates/mods` (data, import, play tree) and `app/src/mods.rs` (launch); the Mods tab is
`app/src/views/library/properties/mods.rs`.

## Using it

- Propiedades → Mods → *Añadir carpeta de mod...*: pick a folder laid out like the game's own folder (its files go
  to the same relative paths). The folder is copied in and never changed; the mod is turned on for the account
  that added it.
- The checkbox turns a mod on or off for this account. A mod turned on goes last and wins a file over the ones
  already on. Changes apply at the next launch; nothing can be changed while the game runs.
- Each enabled mod lists the game's files it replaces and the files it wins or loses against other mods.
- *Quitar* asks first, with the mod's size and how many accounts use it, then deletes it for every account.

## Where things are

Under the library root that holds the install (`<root>/common/<InstallDir>`):

```
<root>/decanter/mods/gog/<game id>/
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
- Verify, repair and update read and write only `common/<InstallDir>`: a mod never writes into the install.
- When the library root is outside the folders the Steam Linux Runtime shares by default, the launch adds it to
  `PRESSURE_VESSEL_FILESYSTEMS_RW`.

## Rules an import keeps

- Only regular files and folders; a link, device or name that is not UTF-8 refuses the whole import.
- Paths: relative, at most 1 024 bytes and 64 components, no `.`, `..`, `\`, `:` or control characters.
- Two names that are the same file for a Windows game (differing only in case) refuse the import.
- `Galaxy64.dll` and `Galaxy64.orig.dll` are kept for decanter's Galaxy hook and refuse the import.
- At most 200 000 files and 64 GiB per mod, and 1 GiB left free on the library's disk.
- Files are copied into `incoming/<pid>-<n>/`, created new (never through an existing file), hashed (SHA-256),
  then renamed into place; a failure removes what was copied.

Every `mod.toml`, set file and play tree record is checked again when read; a mod file that is not a plain file
inside its mod's `files/` fails the launch instead of being linked.

## Not yet

Archives (phase 2), DLL overrides and reordering (phase 3), update checks and the uninstall dialog (phase 4),
external games, renaming a mod and choosing its target folder.
