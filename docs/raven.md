# Raven (experimental)

[Raven](https://useraven.org) ([mirkobrombin/raven](https://github.com/mirkobrombin/raven)) is an
early-stage runtime that runs Windows programs on Linux without Wine. decanter offers it as an optional,
experimental section for Windows executables (decision #1069), separate from the library's games, which keep
running under Proton through umu-run. Nothing about Raven is downloaded or built until the player asks for it.

## Switching it on

Ajustes → General → «Raven (experimental)» → «Mostrar la sección Raven» (setting `raven_enabled` in the
library's database, off by default). With it on, the top bar shows a «Raven» section, marked as being tested.
The section exists only where `Feature::Raven` is offered: Linux on x86_64. The Windows and macOS builds of
decanter show no section (Windows runs Windows programs itself); the same runner interface answers
`Status::Unsupported` there.

## Building

«Construir…» first asks for confirmation, saying what is downloaded and how much it takes. On «Descargar y
construir» decanter:

1. records the confirmation (`runner::confirm`), which the build requires;
2. clones `github.com/mirkobrombin/raven` shallow at the pinned commit (`RAVEN_COMMIT` in
   `crates/runtime/src/runner.rs`, 3179949f14330f14407a41cc923dc902b5aa2bb8, 2026-10-29) and checks that the
   checked-out commit is that one;
3. enters Raven's own nix environment (`crates/runtime/src/os/linux/raven/raven.nix`, through `decanter-nix`:
   the host's tools when they cover it, else the system's nix, else nix-portable): CMake, Ninja, Python 3,
   the host gcc and g++ (pinned in `CC`/`CXX`, since the MinGW compiler's setup hook would otherwise take
   them), pkg-config, libffi, Wayland and wayland-protocols, xkbcommon, pixman, FreeType, libpng, OpenSSL and
   `x86_64-w64-mingw32-gcc`;
4. builds only the runtime, `ninja -k 0 native/raven`, since Raven's full default build stops on a test program
   (`native/vulkan-fixtures/client.exe`, an internal compiler error of nixpkgs' MinGW GCC 16.2.0);
5. marks the build finished with one rename of its marker, after every step passed.

Progress (sources, tools download, configure, Ninja's steps) shows in the section, with «Cancelar»: it drops the
build, kills its process group and removes the folder. A failed build shows its error and the end of its log.
«Eliminar» removes the build, its toolchain, the log and the confirmation; the programs added stay.

Measured in the feasibility spike (#1067): configure about 23 s (CMake downloads Notepad++, SQLite and BusyBox
by hash), the runtime about 3 minutes, a 139 MB `raven` binary; a full build folder of all targets took 5.2 GB.
At the current pin, on a 32-thread machine with the host's nix: configure about 21 s, the runtime about 26 s, a
137 MB `raven` binary and a 613 MB build folder (the runtime target only); `native/raven` now also builds Raven's own
`cmd.exe` and its command-line tools, and needs OpenSSL for its crypto code.

## Programs

«Añadir…» (a file picker) or dropping files on the section adds Windows programs (a PE executable, read by
`app/src/external/probe`, never run, changed or moved). Dropped anywhere else in the window, a program is still
added as an external game of the library ([drop-executable.md](drop-executable.md)). Each program has
«Ejecutar»/«Detener», «Quitar» (its manifest and its prefix) and «Registro», the log of its last run; a run
that fails shows the end of its output under the program.

A program runs through the launch interface's `raven` kind (`app/src/launch/raven.rs`): Raven, started from the
toolchain that built it, as a Linux program whose session is named by the program's prefix, so Detener and
closing decanter reach it like any other game.

## Where its files live

Everything is inside the active library (docs/data-in-library.md):

| Path | What |
| --- | --- |
| `<library>/decanter/runners/raven/<commit>/` | The checkout and its build tree (`build/native/raven`), with `.decanter-built` once finished |
| `<library>/decanter/runners/raven/toolchain/` | Raven's nix environment files, host marker and, if used, nix-portable and its store |
| `<library>/decanter/runners/raven/build.log` | The last build's output |
| `<library>/decanter/runners/raven/confirmed` | The commit the player confirmed |
| `<library>/decanter/runners/raven/manifests/<profile>/` | One manifest per program, in the external games' format (the same code writes a `libraryfolders.vdf` beside `manifests/`) |
| `<library>/decanter/runners/raven/prefixes/<id>/` | Each program's Raven prefix |

## Known not to work yet

- A plain MinGW `hello.exe` fails with `raven: missing PE module: msvcrt.dll`: Raven's guest components
  (`provision-*.py`) are not provisioned yet, and Direct3D needs DXVK and vkd3d-proton besides. No commercial
  game has been tried.
- How Raven is told which prefix to use, and whether `--run-process` starts a GUI program, are assumptions until
  the first real build is tried: they sit in one function, `invocation` in
  `crates/runtime/src/os/linux/raven/mod.rs`, with a `TODO` naming what to confirm.

## Licence

Raven is GPL-3.0-or-later. It is not part of decanter: its source is fetched from GitHub and built on the
player's machine, and it runs as its own process; no Raven source or binary is in a decanter release
([LICENSES.md](../LICENSES.md)).
