# Dropping an executable on the window

Dropping a program's file anywhere on decanter's main window adds it as an
external game (decision #1065): no form, the game is in the library at once
and a notice in the corner says what happened, with «Editar» (the external
game form on that game) and «Deshacer». The dropped file is opened
read-only and never run, changed or moved.

## What is accepted

Each file is classified by its first bytes, not its extension
(`app/src/external/probe/`):

| File | Accepted when | Runs as |
|---|---|---|
| Windows program | `MZ`, a `PE\0\0` header, `IMAGE_FILE_EXECUTABLE_IMAGE` and not `IMAGE_FILE_DLL` | Proton in its own prefix (Linux), itself (Windows) |
| Linux program | ELF `ET_EXEC`, or `ET_DYN` with a `PT_INTERP` header (a PIE), with the permission to run; never a `*.so`/`*.so.N` name | itself, from its folder (Linux only) |
| AppImage | ELF with `AI\x01`/`AI\x02` at offset 8, or an `.AppImage` name | itself; without the permission to run it is added and the notice says to `chmod +x` it, since decanter never changes the file |
| Script | `#!` with the permission to run | itself (Linux only) |

While the Raven section is shown ([raven.md](raven.md)), every dropped file
goes to Raven's programs instead: a Windows program is added there with its
own Raven prefix, anything else is refused in the section's notice, and the
library is untouched.

A folder that `decanter_ps::library::inspect` takes for a PS dump, where the
PS section is offered, goes to the PS section's «Añadir volcado…» instead:
the same Copiar/Mover question, progress and cancel. Only the first dump of a
drop is offered; any other is listed in the notice to add once it is done.

Anything else (`.lnk`, `.msi`, `.bat`, a DLL, a shared library, any other
folder, a device or socket, a file that can't be read) is refused, one line per file in
the notice saying why and which kinds are handled. A Linux program dropped on
the Windows build, or any program on macOS, is refused as a program for
another system. Symlinks are followed to read the target, and the game keeps
the canonical path.

Several files dropped at once are handled one by one and summed up in one
notice; «Deshacer» removes every game that drop added. A file whose canonical
path is already an external game of the profile isn't added again: the
existing game is selected and the notice says so.

## Name

- Windows: `ProductName` from the version resource (`VS_VERSIONINFO`, the
  US English string table if there is one, else the first), else
  `FileDescription`, else `InternalName`. Engine placeholders (`Unity`,
  `Unity Player`, `Unreal Engine`, `BootstrapPackagedGame`) don't count.
- Linux: `Name=` of a `.desktop` file beside the executable whose `Exec=`
  names it (or the only one there); for an AppImage, its file name up to the
  version or architecture (`Game-1.2-x86_64.AppImage` is `Game`).
- Otherwise the nearest of the four folders above the executable whose name
  isn't generic (`bin`, `binaries`, `win64`, `x64`, `game`, `release`,
  `shipping`, `common`...), else the file's stem.

The name is cleaned before use: control and bidirectional override
characters removed, whitespace collapsed, at most 80 characters, never empty.

## Images

- Windows: the first icon group (the icon Explorer shows), its largest image
  up to 256 px. A PNG image is kept as it is; a DIB one is wrapped in a
  one-image `.ico` and decoded with the `image` crate into a PNG.
- Linux: the `.desktop` file's `Icon=` (an absolute path to an image, or a
  name matched against the files beside the executable; system icon themes
  are not searched), else `icon`/`logo` images beside it; `cover`/`capsule`
  for the cover and `hero`/`header` for the background, as PNG, JPEG or ICO.
  An AppImage's own `.DirIcon` is not read (it sits inside its SquashFS).

The images are written to `<library>/decanter/external/<game id>/` and the
game's art points at them with `file://`, the way the form keeps a picked
file. Removing the game, by «Deshacer» or «Eliminar», removes that folder.
Without images the slots stay empty, and the form's art search can fill them.

## Limits

Every read is bounded: at most 2 MiB per read (a resource, an image), at
most 8 MiB per file probed, whatever the headers say. At most 96 sections,
512 entries per resource directory, the three levels of the resource tree
and no deeper (a directory pointing back at itself is read once per level),
64 entries per icon group, 256 blocks in a version resource, 256 ELF program
headers, 512 entries in the executable's folder, 64 KiB per `.desktop` file
and 8 MiB per image beside the executable. Icons are decoded under
`image::Limits` (256 × 256, 64 MiB); images beside the executable must be at
most 4096 × 4096 before they are kept. Special files are refused before they
are opened, and the file is opened non-blocking and checked again.

## Launch

A Linux program runs as itself through the same launch path as a Proton
game (`decanter_runtime::Launch::native`): working directory the program's
folder, the game's own scope and GameMode and MangoHud when they are on, and
the prefix path only as the session's key in `DECANTER_SESSION`, by which
its processes are found, waited for and stopped. Nothing creates that
folder. Whether a game is a Linux program is read from its executable's
first bytes at each launch, so nothing new is stored in the manifest; its
Propiedades → Compatibilidad says it runs without Proton.

## Undo

«Deshacer» removes the games of that drop the same way «Eliminar» does:
manifest, favourites, playtime, collections, the images folder above and
the prefix. A Windows game's prefix is only created when it first runs, so
undoing before playing removes nothing but what the drop wrote. «Deshacer»
is disabled while one of those games is running.
