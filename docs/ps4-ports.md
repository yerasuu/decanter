# PS4 games beyond Bloodborne

Decision (2026-10-10): neither option below. `Port::Bbport` follows
decanter's fork of bbport, [yerasuu/psport](https://github.com/yerasuu/psport),
with `deadinside28/bloodborne_pc` as its upstream remote. Every PS4 dump
that is not Bloodborne is `Status::Experimental` and runs through it with
`BB_SKIP_GAME_CHECK=1` and `BB_PERMISSIVE=1`, without mods.

Why the fork instead of universal_ps4_native: that repo is a one-commit
copy of bbport from 2026-10-05 with no shared history, already behind
bbport's GPU code, and its generalization is small enough to redo on top
of upstream. Both lacked the same pieces (scope-independent import
resolution, event flags), so they are fixed once, in the fork.

What the fork adds over upstream 0.5 (`c7e3e55`):

- `scripts/patches.py`: an empty `patches.bin` for titles other than
  Bloodborne; every built-in patch is a Bloodborne 1.09 address.
- `src/probe.c`: `BB_PERMISSIVE=1` returns 0 from imports the runtime
  lacks, logging each once.
- `src/runtime.c`: imports resolve by NID whatever their `#lib#mod`
  scope, through Bloodborne's scoped names; `src/import_names_all.inc`
  names every NID in shadPS4's aerolib (97623), binary searched.
- `src/runtime_kernel.c`: newer-SDK libc hooks (`_sceKernelSetThreadDtors`,
  `sceLibcHeapGetTraceInfo`, sanitizer replacements).
- `src/runtime_eventflag.c`: kernel event flags.

Atelier Ryza (CUSA16458) gets through libc start, system modules, 32
event flags and its loading threads, then faults at guest offset
`0x609df4` reading a null graphics descriptor.

The comparison that led here follows.

## Before the fork

As of 2026-10-10 decanter ran exactly one PS4 game: Bloodborne, through
bbport. `decanter_ps::status::classify` sent every other PS4 dump to
`Status::Unsupported`. Only `Platform::Ps5` dumps went to AnyPS5.

## Why AnyPS5 can't take PS4 games

AnyPS5 ships the PS5 graphics stack only (`libSceAgc`, `libSceAgcDriver`,
`libSceAgcVsh`). A PS4 `eboot.bin` imports `libSceGnmDriver`, and no host
library in the checkout exports its NIDs. The only PS4 code in AnyPS5 is
`sceSaveDataTransferringMountPs4` and `sceSaveDataDirNameSearchPs4`. Both
exist so a PS5 title can look for PS4 saves, and both report nothing found.

## Option 1: Ryty

[aarvsn/Ryty](https://github.com/aarvsn/Ryty) is a fork of AnyPS5 that
claims PS4 support from v1.0.1 (2026-10-03). GPL-2.0-only. Read at
`63639c1` (v1.0.4, 2026-10-07).

- CLI: `ryty [options] <input.elf> <output>`, with `--ps4`, `--ps5` and
  `--console auto|ps4|ps5`. Other flags match AnyPS5's relinker plus
  `--report <file>` (JSON, schema `ryty-port-report/1`) and `--macos`.
- Runtime layout: same as AnyPS5 (`libs/`, `app0/` beside the executable).
- Environment: `RTY_*` variables. The `ANYPS5_*` names still work.
  The input ini is `ryty-input.ini`.
- Build: `cmake --preset linux-release`, then
  `cmake --build build/linux-release --parallel`.

What the PS4 support is, read from the tree:

- `ElfReader::DetectConsoleTarget` sniffs the file for `libSceGnmDriver`,
  `libkernel.sprx`, `libkernel.prx` or `libScePad.sprx`.
- `RelinkerPipeline` renames `*.sprx` imports to `*.prx`. That is the whole
  PS4 backend: 8 lines in commit `899b8df`.
- `core/libs/prx` has no `libSceGnmDriver` and no other PS4-only library.
  The graphics libraries are the same three Agc ones as AnyPS5.
- `docs/user/COMPATIBILITY.md` lists one title, Dreaming Sarah
  (PPSA02929), a PS5 game. No CUSA title is listed.

Verdict: with Ryty a PS4 executable gets past the import rename and then
has nothing to bind its Gnm calls to. Integrating Ryty would be cheap,
because it maps onto `Port::AnyPs5` almost one to one. It would still not
run a PS4 game until someone writes a Gnm library for it.

## Option 2: universal_ps4_native

[mashedpotato0/universal_ps4_native](https://github.com/mashedpotato0/universal_ps4_native)
generalizes bbport: it forks `deadinside28/bloodborne_pc`, which is
bbport's upstream, so it uses the same `Port::Bbport` lineage.
GPL-2.0-or-later, 25 commits, no releases.

- Graphics: Gnm/Gnmx command buffers translated to Vulkan 1.3, with a Gnm
  driver shim and a shader recompiler under `gpu/`. This is the piece
  Ryty lacks.
- Pipeline: extracts `.pkg` files (a base game plus its update), links
  `eboot.bin` and the bundled `.prx` files into host memory, and emits one
  standalone Linux executable per game:
  `bin/ps4-native compile ./extracted/CUSAXXXXX`.
- System calls it does not implement return success with no effect, so
  titles keep booting.
- Audio through PipeWire/SDL3. Input: DualSense, DualShock 4, generic
  gamepads.
- Build: `git clone --recursive`, then `./setup.sh`, then `./build.sh`.
- Tested titles: Bloodborne and Street Fighter 30th Anniversary
  Collection only.

Fit with decanter: needs its own `Port` variant, since the setup, build
and per-game compile steps don't match either current port. Bloodborne
dump handling in `bbport.rs` (base plus update merge) is a likely starting
point.

## Not pursued

shadPS4 ([shadps4.net](https://www.shadps4.net/), v0.19.0 on 2026-10-01)
is the most compatible option. It is an emulator, not a native port, so
it falls outside the bbport/AnyPS5 model decanter follows.
