# PS4 games beyond Bloodborne

Since 2026-10-10 `Port::Bbport` follows decanter's fork of bbport,
[yerasuu/psport](https://github.com/yerasuu/psport), with
`deadinside28/bloodborne_pc` as its upstream remote. Every PS4 dump
that is not Bloodborne is `Status::Experimental` and runs through it with
`BB_SKIP_GAME_CHECK=1` and `BB_PERMISSIVE=1`, without mods.

The goal is a compatibility layer, as Wine is, not an emulator: the
game's x86-64 code runs natively and the port provides its system
libraries. The fork's own record of what it adds, what it ports from
other projects (with source commit and license) and its diagnostic
variables is `FORK.md` in the fork; this page is decanter's summary.
Upstream releases are merged in as they come (0.51 at `701f0a7`).

Why the fork instead of universal_ps4_native: that repo is a one-commit
copy of bbport from 2026-10-05 with no shared history, already behind
bbport's GPU code, and its generalization is small enough to redo on top
of upstream. Both lacked the same pieces (scope-independent import
resolution, event flags), so they are fixed once, in the fork.

## What the fork adds (as of `618396d`)

Runtime (`src/`):

- Imports resolve by NID whatever their `#lib#mod` scope
  (`runtime_same_nid`); `import_names_all.inc` names every NID in
  shadPS4's aerolib (97623), binary searched.
- `BB_PERMISSIVE=1` returns 0 from imports the runtime lacks, each
  logged once. Before the game starts, every import without
  implementation or answered by a stub (`runtime_stub`) is listed, with a
  summary line.
- Newer-SDK libc hooks, kernel event flags, `ulobjmgr`, display safe
  area, `sceKernelGetdents`, thread stacks kept in their attributes
  (`scePthreadAttrSetstack`/`Getstackaddr`, `pthread_getstack_np`).
- TLS module ids only for modules with TLS, up to 31.
- `BB_AUDIO_STATS=1` reports each audio port's peak and RMS (dBFS) every
  5 s, so a test tells sound from silence without listening.

Libraries ported in (`gpu/`, each first committed as it was at the
source):

- `gpu/ngs2/`: libSceNgs2 from AnyPS5, adapted to PS4 structure
  layouts. Produces silence in Ryza for now (`BB_HLE=libSceNgs2` +
  `BB_AUDIO_STATS` measures it); the user's module is used instead.
- `gpu/fiber/`: libSceFiber from shadPS4, on the game's GS-based TCB.
  Preferred over the user's module (`BB_LLE=libSceFiber` to compare).

System modules: `BB_SYS_MODULES=<folder>` links the user's own dump of
the console's `*.sprx` a title needs, ahead of the port's versions. They
are never shipped and each is a stopgap until the port implements its
library.

Diagnostics and tools: `BB_NGS2_TRACE`, `BB_FIBER_TRACE`,
`BB_DUMP_SKIPPED_GS=<dir>` (GS, copy and ES programs of skipped
stream-out setups), `tools/gcn_dis` (a GCN disassembler on the
recompiler's decoder; LLVM does not disassemble gfx7).

## What decanter does with it

- `Port::Bbport` clones the fork; a checkout cloned from upstream has its
  `origin` moved onto the fork at the next update check.
- A port is rebuilt when its checkout moved since its last build (the
  build marker holds toolchain and commit), and CMake caches left from
  before the PS folder moved are reset.
- Ajustes of the PS section picks the system module folder
  (`sys_modules_dir`, passed as `BB_SYS_MODULES` to experimental titles).
- Each job's whole output goes to `psport/logs/<target>-<action>-<time>.log`,
  headed by decanter's version, the game, the port commit, the command,
  its directory and environment; the last 30 per target are kept.
- Fallos records, once per run, a "Falta" entry listing what the port
  left unimplemented, imports the game actually called first.

## Atelier Ryza (CUSA16458)

With the user's module dump (LLE libSceNgs2, Rtc, Ult): sound, the title
menu, New Game, the prologue's scenes and dialogue, and the field with
the player in control. Tested headless (inside a headless gamescope, with
scripted pad input through `BB_PAD_FILE` and the game's own frame dumps
through `BB_PRESENT_DUMP_TRIGGER`).

Open: about 60000 draws a session use a geometry shader with stream-out
and are skipped (upstream shadPS4 skips them too). Measured: stream 0 to
buffer 0 only, 24-byte records (`VTX_STRIDE` 6 dwords), the copy shader
takes the buffer V# through user-data pointer `s[2:3]`, and every such
draw is bracketed by `STRMOUT_BUFFER_UPDATE` 0x0 (start at offset 0) and
0x19 (store the filled size to guest memory). The draws rasterize nothing
themselves; the vertices they capture feed later draws. Being
implemented with `VK_EXT_transform_feedback`: registers, copy shader
parsing and Vulkan enablement are in the fork; SPIR-V outputs and the
draw-side binding with the filled-size write-back are in progress.
