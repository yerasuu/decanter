# Raven (experimental)

[Raven](https://useraven.org) ([mirkobrombin/raven](https://github.com/mirkobrombin/raven)) is an
early-stage runtime that runs Windows programs on Linux without Wine. decanter offers it as an optional,
experimental section for Windows executables (decision #1069), separate from the library's games, which keep
running under Proton through umu-run. Nothing about Raven is downloaded or built until the player asks for it.

## Switching it on

Ajustes → Avanzado → «Raven (experimental)» → «Mostrar la sección Raven» (setting `raven_enabled` in the
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
   `windres`, `nm` and `dlltool`), and for the installer components GNU Bison (only `provision-installer.py`'s
   tool check asks for it), GNU patch and `widl`. nixpkgs has `widl` only inside Wine (1.6 GiB with its
   closure), so the recipe builds it from `mingw-w64-tools/widl` of the mingw-w64 14.0.0 release its MinGW
   headers come from (the same source tarball, pinned by nixpkgs' hash, about 20 s), with that release's IDL
   files and installed headers in `share/widl/include/`: not in `include/`, which the shell would put in the host
   gcc's search path. Raven's README also lists clang; nothing these scripts run calls it, so the recipe leaves it
   out;
5. runs Raven's provisioning scripts (`COMPILE` in `crates/runtime/src/os/linux/raven/build.rs`), into
   `components/` of the checkout: `provision-rtl.py` (`raven-rtl.dll`, the guest RTL every heap call goes to),
   `provision-controls.py` (the common controls, `raven-user32.dll`, `raven-shell.dll`, DirectWrite, Direct2D,
   `ucrtbase.dll` and Raven's `control.exe`, `regedit.exe`, `dism.exe`, file picker and host bridge),
   `provision-controls.py --arch x86` (the same for 32-bit programs, without the C runtime), and
   `provision-installer.py` and `provision-installer.py --arch x86` (into `installer/` and `installer32/`): the
   Windows Installer engine and what installers lean on, `msi.dll`, `msiexec.exe`, `cabinet.dll`, `sxs.dll`,
   `rpcrt4.dll`, `raven-storage.dll`, `msxml3.dll`, `xmllite.dll`, `atl100.dll`, `oleaut32.dll`, `propsys.dll`,
   `mlang.dll`, `windowscodecs.dll`, `libjpeg.dll`, `libtiff.dll`, `gdiplus.dll`, `urlmon.dll`, `riched20.dll`,
   `msftedit.dll`, `msvfw32.dll`, `advpack.dll`, `raven-setupapi.dll`, `raven-appx.dll`, `mspatcha.dll`,
   `vbscript.dll`, `jscript.dll`, `scrrun.dll` and `wshom.ocx`. The first three download nothing; the installer
   script downloads five upstream releases (below), checks each against the SHA-256 it pins, applies Raven's two
   libxml2 patches (`native/guest/raven-installer/patches/libxml2/`) and writes `provenance.json` with the hashes
   of everything it used and built. All of it is MinGW `gcc`, `windres`, `dlltool` and `widl` over Raven's own
   MIT sources under `native/guest/` and those releases;
6. configures with `-DRAVEN_RTL_COMPONENT_PATH`, `-DRAVEN_CONTROLS_COMPONENT_DIR`,
   `-DRAVEN_CONTROLS32_COMPONENT_DIR`, `-DRAVEN_INSTALLER_COMPONENT_DIR` and `-DRAVEN_INSTALLER32_COMPONENT_DIR`,
   so CMake copies the components beside `raven` (the 32-bit ones into `build/native/x86/`), and builds
   `ninja -k 0 native/raven x86_ucrt_component installer_components_64 installer_components_32`: the runtime, the
   32-bit C runtime, `ucrtbase32.dll`, and the copies of the installer components, none of which `native/raven`
   depends on. Raven's full default build adds its test programs: at the pin it builds (533 steps) but takes
   7.3 GB;
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
With the installer components (2026-11-12, same machine, from an empty checkout, 386 s in all): `widl` built and
the shell entered about 20 s, the RTL and controls about 46 s, the 64-bit installer components 152 s and the 32-bit
ones 95 s (the script compiles mostly one file at a time), configure 26 s, the runtime 47 s (288 steps). A 185 MB
`raven` binary, an 828 MB build folder and 258 MB of components (124 MB per width under `installer/` and
`installer32/`, mostly the unpacked upstream sources and objects; the components themselves are about 10 MB
per width), 1.1 GB for the checkout. Downloaded besides the above: the five upstream releases, about 11 MB; the
mingw-w64 14.0.0 source tarball, 11 MB, from nix's binary cache or the project's SourceForge downloads, checked
against nixpkgs' hash; Bison and patch from nix's binary cache; `widl` adds 19 MB to the store.

### What the installer components download

`provision-installer.py` at the pin fetches these and nothing else; each archive is checked against the SHA-256
in its `UPSTREAM` table before it is unpacked, kept in the component folder as `NAME-VERSION-source.tar.*`, and its
licence notices copied to `licenses/`. The hashes below were also checked on 2026-11-12 against what each project
publishes:

| Library | URL | SHA-256 | Checked against | Licence |
| --- | --- | --- | --- | --- |
| zlib 1.3.1 | `https://zlib.net/fossils/zlib-1.3.1.tar.gz` | `9a93b2b7dfdac77ceba5a558a580e74667dd6fede4585b91eefb60f03b72df23` | Mark Adler's GPG signature (key `5ED46A6721D365587791E2AA783FCD8E58BCAFBA`) of the [v1.3.1 release](https://github.com/madler/zlib/releases/tag/v1.3.1), byte-identical | [zlib License](https://zlib.net/zlib_license.html) |
| libjpeg 10 (IJG) | `https://www.ijg.org/files/jpegsrc.v10.tar.gz` | `8b9eaa13242690ebd03e1728ab1edf97a81a78ed6e83624d493655f31ac95ab5` | the [IJG](https://www.ijg.org/) publishes no checksum or signature: only that its own server serves this file | IJG License (its `README`, "LEGAL ISSUES") |
| LibTIFF 4.7.1 | `https://download.osgeo.org/libtiff/tiff-4.7.1.tar.gz` | `f698d94f3103da8ca7438d84e0344e453fe0ba3b7486e04c5bf7a9a3fabe9b69` | Even Rouault's GPG signature (`tiff-4.7.1.tar.gz.sig`, key `B1FA7D81EEB8E66399178B9733EBBFC47B3DD87D`) | [libtiff License](https://libtiff.gitlab.io/libtiff/project/license.html) |
| libxml2 2.12.8 | `https://download.gnome.org/sources/libxml2/2.12/libxml2-2.12.8.tar.xz` | `43ad877b018bc63deb2468d71f95219c2fac196876ef36d1bee51d226173ec93` | GNOME's `libxml2-2.12.8.sha256sum` | MIT (its `Copyright`) |
| libxslt 1.1.42 | `https://download.gnome.org/sources/libxslt/1.1/libxslt-1.1.42.tar.xz` | `85ca62cac0d41fc77d3f6033da9df6fd73d20ea2fc18b0a3609ffb4110e1baeb` | GNOME's `libxslt-1.1.42.sha256sum` | MIT (its `Copyright`) |

No Microsoft binary is downloaded: the libraries the components link against are MinGW-w64's import libraries.
The build tools come from the pinned nixpkgs: `widl` from mingw-w64 14.0.0 (LGPL-2.1-or-later, Wine's code; the
IDL files and headers under mingw-w64's ZPL-2.1 and their own notices), GNU Bison 3.8.2 and GNU patch 2.8
(GPL-3.0-or-later).

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
| With `-mcrtdll=ucrt`, linked against MinGW's `msi`, `rpcrt4` and `cabinet` import libraries: `UuidCreate` and `UuidToStringA`, `FDICreate` and `FDIDestroy`, `MsiOpenDatabaseA` creating a database, `CREATE TABLE`, `INSERT`, `MsiDatabaseCommit`, then reopening it read-only and a `SELECT` through `MsiDatabaseOpenViewA`, `MsiViewExecute` and `MsiViewFetch` | Runs: a fresh UUID, every call 0, `Value = 42`, exit 0 | Runs the same, exit 0 |

The i686 results are with decanter's patch; `printf`, `fprintf` and `fputs` to `stdout` and `stderr`, `%zu`, `%Iu` and
`%td` among other arguments, and `fopen`/`fgets`/`ftell` on a file were checked in both widths and both C runtimes.

At the previous pin (3179949f14) only a program that needed no C runtime and no heap ran: `printf` stopped at
`missing PE module: msvcrt.dll`, and with a `ucrtbase.dll` at `unimplemented native API GetProcessHeap`, since
neither `raven-rtl.dll` nor the C runtime was built. The owner's 32-bit installer stopped at `unimplemented native
x86 API InitCommonControls` and then `unimplemented native process API` (the line that ends every such run, naming
no function); the common controls now answer it for both widths. A run that stops on a 32-bit function shows it
under the program like a 64-bit one. Without the installer components the same Windows Installer program stops
at `missing PE module: cabinet.dll`; with them its database round trip runs for both widths. No installer has been
run against them yet, so whether the owner's 32-bit installer now gets past `unimplemented native process API` is
still to be checked.

Still missing from decanter's build:

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
`raven-shell` and the rest), each file marked `SPDX-License-Identifier: MIT` (Raven's `LICENSES/MIT.txt`). The
installer components add the five upstream releases above, downloaded from each project's own site on the
player's machine and compiled there, under their own licences (zlib License, IJG License, libtiff License, MIT
for libxml2 and libxslt), whose notices `provision-installer.py` copies beside the components; decanter
distributes none of them. decanter's patch ([Patches](#patches)) is applied to that checkout on the player's machine;
the patch file is GPL-3.0-or-later like Raven, and the file it changes, `native/guest/raven-crt/ucrtbase.c`, is
`SPDX-License-Identifier: MIT` and stays MIT.
