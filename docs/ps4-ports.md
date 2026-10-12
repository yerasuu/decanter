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

What each title imports, what the port lacks and what to implement next:
[ps4-import-matrix.md](ps4-import-matrix.md) (a dated snapshot).

## What the fork adds (as of `7f56232`)

The fork follows its own `CLAUDE.md`: no game- or shader-specific hacks,
no tests (the test suite inherited from bbport was removed), comments only
where needed. `tools/regress.sh` is how progress is measured.

Runtime (`src/`):

- Imports resolve by NID whatever their `#lib#mod` scope
  (`runtime_same_nid`); `import_names_all.inc` names every NID in
  shadPS4's aerolib (97623), binary searched.
- `BB_PERMISSIVE=1` returns 0 from imports the runtime lacks, each
  logged once. Before the game starts, every import without
  implementation or answered by a stub (`runtime_stub`) is listed, with a
  summary line.
- Imports with output arguments (`import_outparams.inc`, generated from
  shadPS4's prototypes by `tools/import_outparams.py`, 809 NIDs) are
  logged naming the arguments a permissive 0 leaves unwritten;
  `BB_PERMISSIVE_OUT=error` makes them return `0x80020016` instead.
- Modules a game loads by path (Unity's `Media/Modules/*.prx` and
  `Media/Plugins/*.prx`) are prelinked by `link_modules.py`, unstarted,
  and started on `sceKernelLoadStartModule`; `sceKernelDlsym` hashes
  names to NIDs; module info for unwinding (`src/runtime_module.c`).
  Modules the game bundles in `sce_module` (such as its own
  `libSceNpToolkit2.prx`) are linked whenever something imports their
  library.
- Exceptions: `sceKernelInstallExceptionHandler`/`RaiseException` deliver
  to the target thread through `SIGRTMIN+3`, with the PS4 ucontext, on the
  thread's own stack — Unity's garbage collector suspends threads this
  way. Raises are held back while the thread holds a runtime lock.
- libkernel and POSIX surface: Neo mode, SDK version, cpumode, UTC/local
  time conversion (with the zone libc's `mktime` reads), condattr,
  rwlocks, `sem_*`, detach, cancel state, mutex protocols, sched params,
  direct memory query and checked release, mlock, utimes, UUIDs.
- Newer-SDK libc hooks, kernel event flags, `ulobjmgr`, display safe
  area, `sceKernelGetdents`, thread stacks kept in their attributes,
  `scePthreadSem*` on kernel semaphores, main direct memory, the guest's
  `/dev/urandom` and `/dev/random`, Sysmodule reference counting, DiscMap,
  SystemService parameters, save data umount-with-backup events,
  `scePadGetHandle`.
- The Bloodborne game menu's patches only apply to Bloodborne.
- `BB_PAD_HOST=0` keeps physical controllers closed (scripted headless
  runs set it).
- The libc heap settings are read from `SceProcParam` +0x38 (upstream
  read +0x30, the main thread's stack size, and newer SDKs crashed there).
- TLS module ids only for modules with TLS, up to 31.
- `BB_AUDIO_STATS=1` reports each audio port's peak and RMS (dBFS) every
  5 s, so a test tells sound from silence without listening.

GPU (`gpu/`):

- Geometry shader stream-out through `VK_EXT_transform_feedback`
  (copy shader parsing, SPIR-V `Xfb` outputs, draw-side binding with the
  filled-size write-back).
- Point-list geometry shaders declare `OutputPoints` (shadPS4 emitted a
  bare `OutputVertices`, an invalid module).
- AvPlayer (inherited shadPS4 code): loops by seeking the streams only,
  ends a loop that reads nothing instead of spinning, and no longer locks
  itself (`Start` called `Stop` under its own lock; game callbacks now run
  with no lock held). The first two bugs are also in upstream shadPS4.

Libraries ported in (each first committed as it was at the source):

- `gpu/ngs2/`: libSceNgs2 from AnyPS5, adapted to PS4 structure
  layouts and voice event numbers, with downmix, float PCM, VAG waveforms
  and the game's UserFx handlers (sampler voices and custom rack module
  `0x19`). Matches the console module in Ryza and Ayesha within a fraction
  of a dB, so it is used instead of the user's.
- `gpu/fiber/`: libSceFiber from shadPS4, on the game's GS-based TCB.
  Preferred over the user's module (`BB_LLE=libSceFiber` to compare).
- `gpu/font/`: libSceFont and libSceFontFt from shadPS4 over FreeType,
  with `sceFontGlyphRenderImage` and `sceFontCloseFont` written for the
  port. System fonts from `BB_SYS_FONTS`; otherwise the host's Noto fonts
  (`BB_FALLBACK_FONTS` adds folders), else the embedded DejaVu (Latin).
  FreeType is now a build dependency, so decanter's host check requires
  `freetype2`.
- `gpu/audiodec/`: libSceAudiodec from AnyPS5 (shadPS4 has none): AAC and
  MP3 on libavcodec, ATRAC9 on LibAtrac9. Ryza's title menu music needs it.

System modules: `BB_SYS_MODULES=<folder>` links the user's own dump of
the console's `*.sprx` a title needs. They are never shipped and each is
a stopgap until the port implements its library: the libraries the port
implements (libkernel, GnmDriver, VideoOut, Pad, AudioOut, Fiber, Font,
Audiodec, Ngs2, Ult, Rtc) win over the dump unless `BB_LLE` names them.
Since 2026-10-12 none of the seven titles needs the dump: with it set, no
`.sprx` from it is linked.

Diagnostics and tools: `BB_NGS2_TRACE`, `BB_FIBER_TRACE`,
`BB_DUMP_SKIPPED_GS=<dir>`, `tools/gcn_dis` (a GCN disassembler on the
recompiler's decoder; LLVM does not disassemble gfx7), and
`tools/regress.sh`: runs every listed title inside a headless gamescope
(no window, no sound) and reports how far each got (loaded, entered,
initialized, first frame, drawn frames, audio) and its crash signature,
flagging regressions against a previous run.

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
- A game's row and cover have a right-click menu: play or stop, details,
  merge the 1.09 update, the full log, the dump's folder, copy the title
  ID.

## Titles

All tested headless (a headless gamescope, scripted pad input through
`BB_PAD_FILE`, the game's own frame dumps through
`BB_PRESENT_DUMP_TRIGGER`). SaGa draws its dialogue text since the
buffer write traps (CPU writes to buffer pages in VRAM were missed with
guest memory in place).

`tools/regress.sh` on 2026-10-12, 06:30: six of the seven reach drawn
frames with audio, the seventh (ALTDEUS) the GPU without visible frames.

| Title | Reaches | Imports without implementation |
|---|---|---|
| Bloodborne | frames, audio | 6 |
| Atelier Ryza | frames, audio | 61 |
| Atelier Ayesha DX | frames, audio | 56 |
| Tales of Symphonia Remastered | frames, audio | 348 |
| Astrea | frames, audio | 348 |
| SaGa Emerald Beyond | frames, audio | 329 |
| ALTDEUS: Beyond Chronos | GPU, black frames (needs PS VR) | 325 |

### Atelier Ryza (CUSA16458, Gust)

Sound (the title menu's music since libSceAudiodec), the title menu, New
Game, the prologue's scenes and dialogue, and the field with the player in
control. Its stream-out draws (about 60000
a session: stream 0 to buffer 0, 24-byte records, bracketed by
`STRMOUT_BUFFER_UPDATE` 0x0 and 0x19) are captured through transform
feedback instead of skipped; the scenes tested look the same either way.

### Atelier Ayesha DX (CUSA16921, PhyreEngine/KTGL)

With the libSceFont port: intro movie, title menu, the opening scenes
with their dialogue text, the field, the narration, the atelier's
tutorials and free control inside the atelier, with sound. The narration
loops a movie, and AvPlayer used to hang there: on looping it rewound the
byte stream under FFmpeg's mp4 demuxer before seeking, after which every
read returned end of file (upstream shadPS4 has the same code). It now
seeks the streams only (`604f623`).

### Unity IL2CPP: Tales of Symphonia Remastered (CUSA50989), Astrea (CUSA49515), SaGa Emerald Beyond (CUSA43027), ALTDEUS: Beyond Chronos (CUSA26563)

The same engine build in all four. They load `Il2CppUserAssemblies.prx`
and `PS4Util.prx` through `sceKernelLoadStartModule` and run with the
garbage collector suspending threads through delivered exceptions.

- **Symphonia:** fmod logo, title screen, the first room with the
  character. It aborted on AvPlayer's self-lock before.
- **Astrea:** the intro's story panels, advanced with cross. It spun
  forever in libc's `mktime` (the zone struct our time conversion left
  unwritten), then blocked on a save-data event.
- **SaGa Emerald Beyond:** title screen and the first story scenes with
  their dialogue text, with the pad read. It crashed after its own
  NpToolkit2 module, never linked, answered nothing.
- **ALTDEUS:** requires a PlayStation VR headset (`param.sfo` sets
  `require_ps_vr`). With libSceHmd reporting no headset, its setup dialog
  is cancelled and the game reopens it forever on a black screen; it has
  no flat-screen mode.

All four probe `/archive/mount/point/Media/...` first and fall back to
`/app0`.
