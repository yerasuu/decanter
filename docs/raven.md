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
   `crates/runtime/src/runner.rs`, 82f124e2fbeab6a5fa3050df13160d12aec38ec1, Raven's `main` on 2026-11-12,
   decision #1655) and checks that the checked-out commit is that one;
3. applies decanter's patch to Raven's sources (`git apply`, see [Patches](#patches)), only at that commit, and
   fails the build if it does not apply;
4. enters Raven's own nix environment (`crates/runtime/src/os/linux/raven/raven.nix`, through `decanter-nix`:
   the host's tools when they cover it, else the system's nix, else nix-portable): CMake, Ninja, Python 3,
   the host gcc and g++ (pinned in `CC`/`CXX`, since the MinGW compiler's setup hook would otherwise take
   them), pkg-config, libffi, Wayland and wayland-protocols, xkbcommon, pixman, FreeType, libpng, OpenSSL and
   the MinGW compilers for both widths, `x86_64-w64-mingw32-gcc` and `i686-w64-mingw32-gcc` (with their
   `windres`, `nm` and `dlltool`);
5. runs Raven's provisioning scripts that download nothing (`COMPILE` in
   `crates/runtime/src/os/linux/raven/build.rs`), into `components/` of the checkout: `provision-rtl.py`
   (`raven-rtl.dll`, the guest RTL every heap call goes to), `provision-controls.py` (the common controls,
   `raven-user32.dll`, `raven-shell.dll`, DirectWrite, Direct2D, `ucrtbase.dll` and Raven's `control.exe`,
   `regedit.exe`, `dism.exe`, file picker and host bridge) and `provision-controls.py --arch x86` (the same for
   32-bit programs, without the C runtime). All of it is MinGW `gcc` and `windres` over Raven's own MIT sources
   under `native/guest/`;
6. configures with `-DRAVEN_RTL_COMPONENT_PATH`, `-DRAVEN_CONTROLS_COMPONENT_DIR` and
   `-DRAVEN_CONTROLS32_COMPONENT_DIR`, so CMake copies the components beside `raven` (the 32-bit ones into
   `build/native/x86/`), and builds `ninja -k 0 native/raven x86_ucrt_component`: the runtime and the 32-bit C
   runtime, `ucrtbase32.dll`, which `native/raven` does not depend on. Raven's full default build adds its test
   programs: at the pin it builds (533 steps) but takes 7.3 GB;
7. marks the build finished with one rename of its marker, after every step passed.

Progress (sources, tools download, configure, Ninja's steps) shows in the section, with «Cancelar»: it drops the
build, kills its process group and removes the folder. A failed build shows its error and the end of its log.
«Eliminar» removes the build, its toolchain, the log and the confirmation; the programs added stay.

Measured in the feasibility spike (#1067): configure about 23 s (CMake downloads Notepad++, SQLite and BusyBox
by hash), the runtime about 3 minutes, a 139 MB `raven` binary; a full build folder of all targets took 5.2 GB.
At the current pin, on a 32-thread machine with the host's nix: configure about 21 s, the runtime about 26 s, a
137 MB `raven` binary and a 613 MB build folder (the runtime target only); `native/raven` now also builds Raven's own
`cmd.exe` and its command-line tools, and needs OpenSSL for its crypto code.
At 82f124e2fb, same machine, from an empty checkout: the provisioning about 50 s (the RTL 1 s, the 64-bit controls
32 s, the 32-bit ones 16 s), configure about 26 s, the runtime about 46 s (231 steps), 118 s in all; a 185 MB
`raven` binary, a 808 MB build folder and 11 MB of components (855 MB for the checkout). The download: the shallow
clone about 7 MB, CMake's hash-pinned test data (Notepad++, SQLite, BusyBox) about 15 MB, and from nix's binary
cache the i686 MinGW toolchain besides what the previous pin used (about 630 MB in the store).

## Programs

«Añadir…» (a file picker) or dropping files on the section adds Windows programs (a PE executable, read by
`app/src/external/probe`, never run, changed or moved). Dropped anywhere else in the window, a program is still
added as an external game of the library ([drop-executable.md](drop-executable.md)). Each program has
«Ejecutar»/«Detener», «Quitar» (its manifest, its prefix and the `.apps` and `.layers` folders beside it) and «Registro», the log of its last run; a run
that fails shows the end of its output under the program.

A program runs through the launch interface's `raven` kind (`app/src/launch/raven.rs`): Raven, started from the
toolchain that built it, as a Linux program whose session is named by the program's prefix, so Detener and
closing decanter reach it like any other game. The command (`invocation` in
`crates/runtime/src/os/linux/raven/mod.rs`) is the one Raven's README gives, checked against `native/main.cpp`
at the pin, which reads the options in this order:

```sh
RAVEN_PREFIX=<prefix> raven --run-process --execution=native --timeout-ms=86400000 [--desktop=wayland] \
    --stdio=inherit program.exe
```

- `RAVEN_PREFIX` is the only way Raven is told its prefix (its `C:` drive, `registry.dat`, fonts); Raven creates
  it on the first run.
- `--timeout-ms`: without it Raven kills the program after 5 s (`native entry timed out`); 86400000 (24 h) is the
  longest it takes, and what it gives a console program started from a terminal.
- `--stdio=inherit`: without a terminal Raven keeps a console program's output to itself and prints only
  `Native process exited with status N`; with it the output reaches the run's log. The program's exit status is
  Raven's.
- `--desktop=wayland` only when `WAYLAND_DISPLAY` is set; Raven connects to Wayland only once the program shows
  a window. The recipe builds no X11 desktop (no libxcb), so on X11 alone a window has nowhere to go and the log
  says so.

A failed run that ends on Raven's `missing PE module: X` or `unimplemented native API X` also says, in the
player's language, which library or function stopped it.

## Where its files live

Everything is inside the active library (docs/data-in-library.md):

| Path | What |
| --- | --- |
| `<library>/decanter/runners/raven/<commit>/` | The checkout and its build tree (`build/native/raven`), with `.decanter-built` once finished |
| `<library>/decanter/runners/raven/<commit>/components/` | The guest components the provisioning scripts compiled, copied by CMake beside `build/native/raven` |
| `<library>/decanter/runners/raven/toolchain/` | Raven's nix environment files, host marker and, if used, nix-portable and its store |
| `<library>/decanter/runners/raven/build.log` | The last build's output |
| `<library>/decanter/runners/raven/confirmed` | The commit the player confirmed |
| `<library>/decanter/runners/raven/manifests/<profile>/` | One manifest per program, in the external games' format (the same code writes a `libraryfolders.vdf` beside `manifests/`) |
| `<library>/decanter/runners/raven/prefixes/<id>/` | Each program's Raven prefix |

## What runs and what does not yet

Measured on 2026-11-12 with decanter's build steps at 82f124e2fb (a scratch build outside the library), console
programs only, no window, compiled with nixpkgs' MinGW GCC 16.2.0 and run with the command above:

| Program | x86_64 | i686 |
| --- | --- | --- |
| `printf`, `puts`, `fprintf(stderr)`, `malloc`/`calloc`/`free`, `HeapAlloc` on `GetProcessHeap`, exit status 7, importing `msvcrt.dll` (MinGW's default) | Runs: all its output, exit 7, about 50 ms | Runs: all its output, exit 7 (without the patch only `puts` and `WriteFile` reached the output) |
| the same with `-mcrtdll=ucrt` (imports `api-ms-win-crt-*`) | Runs: all its output, exit 7 | Runs: all its output, exit 7, about 230 ms (without the patch `%zu` printed 8 bytes of the stack) |
| `InitCommonControls` and `InitCommonControlsEx`, then `CreateProcessA` of one of the above, waited, its exit status read | Runs, the child's output and exit status 7 | Runs, a 32-bit or 64-bit child alike |

The i686 results are with decanter's patch; `printf`, `fprintf` and `fputs` to `stdout` and `stderr`, `%zu`, `%Iu` and
`%td` among other arguments, and `fopen`/`fgets`/`ftell` on a file were checked in both widths and both C runtimes.

At the previous pin (3179949f14) only a program that needed no C runtime and no heap ran: `printf` stopped at
`missing PE module: msvcrt.dll`, and with a `ucrtbase.dll` at `unimplemented native API GetProcessHeap`, since
neither `raven-rtl.dll` nor the C runtime was built. The owner's 32-bit installer stopped at `unimplemented native
x86 API InitCommonControls` and then `unimplemented native process API` (the line that ends every such run, naming
no function); the common controls now answer it for both widths. A run that stops on a 32-bit function shows it
under the program like a 64-bit one.

Still missing from decanter's build:

- the installer services (`msi.dll`, `cabinet.dll`, `sxs.dll`, `rpcrt4.dll`, `msxml3.dll` and others,
  `provision-installer.py`), which need `clang`, `widl`, `bison` and `patch` and download pinned upstream sources
  (zlib, libjpeg, libtiff, libxml2, libxslt from their projects' sites); a program stops at its first call into
  them, so Windows Installer packages and most setup programs do not install yet;
- Direct3D through DXVK and vkd3d-proton (`provision-graphics.py`), the Microsoft root certificates
  (`provision-roots.py`) and Skia with its fonts (`provision-text.py`), which all download;
- the components only Raven's full build makes (`mscoree.dll`, ICU, `l_intl.nls` and others).

Programs take their arguments after `--`; anything between the program and `--` is read as a further PE module
to load (`native/main.cpp`), so decanter passes none. Raven keeps two folders beside each prefix,
`<prefix>.apps` and `<prefix>.layers`. No GUI program, installer or game has been run against this pin.

## Patches

decanter carries fixes to Raven's sources that Raven does not have yet, in
`crates/runtime/src/os/linux/raven/patches/` (unified diffs against `RAVEN_COMMIT`, embedded in decanter). They are
applied to the pinned commit only: a newer commit chosen in the section is built as Raven has it. A patch is dropped
when the pin moves to a commit that has its fix, or rewritten if the pin moves and it no longer applies.

`x86-crt.patch` changes Raven's C runtime component, `native/guest/raven-crt/ucrtbase.c` (`ucrtbase.dll` and the
32-bit `ucrtbase32.dll`, which also answers for a 32-bit program's `msvcrt.dll`):

- The stream record (`Stream`) was 36 bytes on x86. Raven exports its table of standard streams as `_iob` and from
  `__iob_func`/`__p__iob`, and programs built against msvcrt reach `stdout` and `stderr` as `&_iob[1]` and
  `&_iob[2]`, at the stride of x86 msvcrt's `FILE`, 32 bytes. Those pointers fell inside the wrong record, whose
  `handle` field was another field's bytes, so every write through `stdout` or `stderr` went to an invalid handle and
  was dropped without an error; `puts` and the ucrt's `__acrt_iob_func` index the table themselves, which is why they
  worked. The two read-buffer counters are now 16-bit (never above the 4096-byte buffer), which makes the record 32
  bytes on x86 and leaves it at 48 on x64, with a static assertion for each width. A stream opened by `fopen` keeps
  its lock right after the record, where msvcrt's `_FILEX` keeps it, in both widths.
- The formatter read `%zu`, `%zd`, `%td` and `%Iu` as 64-bit everywhere. `size_t` and `ptrdiff_t` are as wide as a
  pointer, so on x86 they took 8 bytes, printed the next argument's bytes with them and shifted every later
  argument. They now follow the pointer width; `%jd` (`intmax_t`) and `%I64d` stay 64-bit.

A missing flush at exit was not the cause: Raven's streams write through to their handle on every call (`fflush`
does nothing), and output written without a `fflush` before `return` is there.

## Licence

Raven is GPL-3.0-or-later. It is not part of decanter: its source is fetched from GitHub and built on the
player's machine, and it runs as its own process; no Raven source or binary is in a decanter release
([LICENSES.md](../LICENSES.md)). The guest components its provisioning compiles come from the same checkout,
from Raven's own sources under `native/guest/` (`raven-rtl`, `raven-crt`, `raven-comctl32`, `raven-user32`,
`raven-shell` and the rest), each file marked `SPDX-License-Identifier: MIT` (Raven's `LICENSES/MIT.txt`); nothing
else is downloaded for them. decanter's patch ([Patches](#patches)) is applied to that checkout on the player's machine;
the patch file is GPL-3.0-or-later like Raven, and the file it changes, `native/guest/raven-crt/ucrtbase.c`, is
`SPDX-License-Identifier: MIT` and stays MIT.
