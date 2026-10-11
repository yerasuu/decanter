# Import coverage matrix: seven PS4 titles on psport master

A snapshot, built 2026-10-11 against psport `master` at `95d88f0`: it ages with every library
the fork gains (module loading for the Unity titles landed right after it, at `1929daa`). The
one-off scripts and run logs behind it were not kept; the method below is enough to redo it.

## How the data was gathered

- **Full import list**: the startup report lists only imports that are a stub or have no
  implementation (`src/probe.c`, `import_state` 3 and 4), plus a summary line with counts for host,
  game modules and system modules. So the complete list comes from the ELF import tables of the
  eboot and every bundled `.prx` (with `scripts/link_modules.py`'s parser),
  keyed by NID; each import's status comes from the reports.
- **Master runs**: 40 s runs in a headless gamescope, end
  schedule only, `BB_PERMISSIVE=1` everywhere (also Bloodborne, so its gaps log instead of stopping),
  `BB_SKIP_GAME_CHECK=1` except Bloodborne, once without and once with
  `BB_SYS_MODULES=.../Bloodborne/sys_modules`. On master only the eboot, `libc.prx` and
  `libSceFios2.prx` are linked at start, so these reports cover those three modules. Ayesha was
  rerun after the last master rebuild: identical report.
- **Modules linked later**: Bloodborne, Ryza and Ayesha: CPU-only reports linking every bundled
  `.prx`. Unity titles overflow the boot file that way ("boot file limits
  exceeded"), so their `sce_module/*.prx` come from a CPU-only run linking just those
  and their `Media/` modules (Il2CppUserAssemblies, plugins) from the dynmod
  worktree build's report, which loads them when the game asks. CPU-only runs lack
  the GPU-side HLE, so NIDs registered under `gpu/` (`LIB_FUNCTION`, `FONT_FUNCTION`,
  `FIBER_FUNCTION`) are counted as host there, except `gpu/bbnet`: master's `out/` has no
  `libbbnet.so` (its dependencies are missing), so its NP/network NIDs keep the report's status.
  (A first version counted bbnet as host and hid ~130 NP/network gaps of the NpToolkit2-based
  titles; fixed in review.)
- An import exported by any bundled module counts as resolved by a game module, even where the run
  that reported it had not linked that module (e.g. UnityNpToolkit2's 1673 imports of the bundled
  `libSceNpToolkit2.prx`).
- **sys LLE only**: missing without sys modules, resolved with them. The Unity `sce_module/`
  and `Media/` runs had no sys-module variant, so there a gap counts as sys LLE when a dump
  module that `link_modules.py` would link exports its NID.
- **Called**: `Runtime: unsupported import ... returns 0` lines, deduplicated, in first-call order.
  Stubs are not logged when called, so "called" means called-and-missing.
- **shadPS4**: every `LIB_FUNCTION` in `src/core/libraries/**.cpp` of the reference clone; a
  registration counts as a stub when its function body logs `(STUBBED)`. Per NID: `yes` real body,
  `stub`, `no` not registered.

## Key findings

- **The gaps that are called are few and early.** Across all runs only 10 to 18 distinct missing
  imports are ever called per title; most of the 80 to 450 gaps per title are imported and never
  reached in the first 40 s (VR, camera, network, NP and dialog libraries).
- **libkernel is the one library every title has gaps in**, and almost all of the early calls are
  libkernel: `sceKernelIsNeoMode`, `sceKernelIsAddressSanitizerEnabled`,
  `scePthreadCondattrInit/Destroy`, `sceKernelConvertUtcToLocaltime` (Gust and Unity),
  `sceKernelSetVirtualRangeName`, `sceKernelDirectMemoryQuery`, `sceKernelGetCpumode`,
  `scePthreadGet/Setschedparam` (Unity). shadPS4 has a real body for nearly all of them.
- **Unity titles share 420 of their 432-452 gaps** (same Unity player eboot and bundled
  NpToolkit2). After dynmod's module
  loading they call, in order: libkernel boot queries, `pthread_setcancelstate`,
  `pthread_detach`, `pthread_mutexattr_setprotocol`/`pthread_rwlock_init` (libScePosix),
  `sceKernelInstallExceptionHandler` then `sceKernelRaiseException` (libkernel_unity). SaGa and
  ALTDEUS log `sceKernelRaiseException` as their last call before stopping; Symphonia and Astrea
  get past it to rendering. libkernel_unity (3 functions in shadPS4's
  `kernel/threads/exception.cpp`) is the Unity-specific blocker after module loading.
- **libScePosix: 36-39 missing functions in each Unity title**, none in the others: pthread
  (detach, cancel, setcancelstate, rwlock init/destroy, sched params, mutexattr protocol), POSIX
  semaphores, sched priority range, and BSD sockets. shadPS4 registers all but `fcntl`.
- **Gust (Ryza, Ayesha) share 82 gaps**: libkernel (22), libSceAudio3d (15), libSceUlt (6, covered
  only by the user's sprx; shadPS4 has no Ult), libSceAudiodec (6, `sceAudiodecInitLibrary` called;
  shadPS4 has no libSceAudiodec either), and boot-time init calls that only need to succeed:
  libSceScreenShot (4 called), `sceGameLiveStreamingInitialize`, `sceSharePlayInitialize`,
  `sceSystemServiceDisableMusicPlayer`.
- **Ayesha's libSceFont imports are all resolved on master** (27 libSceFont + 2 libSceFontFt functions, host, since the font
  port merged); its remaining called gaps are the shared libkernel/SystemService/ScreenShot set,
  `sceSysmoduleLoadModuleInternalWithArg`, libSceDiscMap's `Func_7C980FFB0AA27E7A` and
  `sceSaveDataClearProgress`.
- **Bloodborne calls nothing missing**: 9 missing, all libkernel (`__Ux86_64_setcontext`,
  `sigreturn`, `_is_signal_return`, `__elf_phdr_match_addr`, `__progname`,
  `__pthread_cxa_finalize`, `sceKernelMlock`, `sceKernelGetModuleInfoFromAddr`, `msync`), and 70
  stubs (NP, network, voice, Http), none logged as called.
- **The user's sys module dump matters mostly for Gust**: Ult (7 functions, all Ryza/Ayesha gaps
  through it; `sceUltInitialize` is Ryza's very first unsupported call without it) and one Rtc
  function (`sceRtcParseDateTime`, imported by the bundled NpToolkit2 of Ryza and all four Unity
  titles, never called). The Unity eboots, libc and Fios2 import none of the dumped libraries, so
  their master sys runs link nothing. With the dump linked, its modules' own imports add libkernel gaps:
  `sceKernelGetCompiledSdkVersion` (called, Ryza and Ayesha) and `sceKernelConvertUtcToLocaltime`
  (called, Bloodborne).
- **shadPS4 has a real body for about two thirds of every title's gaps** (Unity 303-318 of
  432-452, Ryza 183 of 271, Ayesha 56 of 93, Bloodborne 57 of 79) and a stub for most of the rest; the ones it lacks are
  libSceUlt, libSceAudiodec, libSceSocialScreen(Dialog), libkernel internals
  (`__elf_phdr_match_addr`, `__progname`, `__pthread_cxa_finalize`, `_is_signal_return`,
  `_sceKernelRtldThreadAtexitIncrement/Decrement`, `sceKernelInternalMemoryGetModuleSegmentInfo`),
  `sceVideoOutConfigureOptionsInitialize_`, libScePosix `fcntl` and the NpInGameMessage family.

## Implement next

Ranked by titles unblocked, then by how early in boot the functions are called. "Unblocks" here
means the call happens on the boot path today (under `BB_PERMISSIVE` it returns 0 and the game
continues on a wrong answer).

1. **libkernel boot queries and pthread attrs** (6 titles: Ryza, Ayesha, all Unity; called first
   in every non-Bloodborne run). `sceKernelIsNeoMode`, `sceKernelIsAddressSanitizerEnabled`,
   `scePthreadCondattrInit/Destroy`, `sceKernelConvertUtcToLocaltime/LocaltimeToUtc`,
   `scePthreadGetname`, `_nanosleep`, `pthread_rwlock_rdlock/wrlock/unlock`, `sceKernelUtimes`,
   `sceKernelMlock`, `sceKernelGetModuleInfoForUnwind/FromAddr`, and for the sys-module path
   `sceKernelGetCompiledSdkVersion`. Small, all in shadPS4.
2. **libkernel Unity memory and thread set** (4 Unity; called on boot):
   `sceKernelSetVirtualRangeName`, `sceKernelDirectMemoryQuery`, `sceKernelGetCpumode`,
   `scePthreadGet/Setschedparam`, then the rest of the 40 (Sleep, CheckedReleaseDirectMemory,
   `__pthread_cleanup_push/pop_imp`, UuidCreate, SetGPO, AttrGetstack, Getthreadid).
   `sceKernelLoadStartModule/Dlsym/StopUnloadModule` are the dynmod job's.
3. **libkernel_unity** (4 Unity): `sceKernelInstallExceptionHandler`, `sceKernelRaiseException`
   (+ `RemoveExceptionHandler`). Called right after module loading; SaGa and ALTDEUS stop there.
   Needs a real signal-based delivery to the target thread, not a return-0 stub.
4. **libScePosix pthread/semaphore/sched part** (4 Unity, ~20 functions; `pthread_setcancelstate`,
   `pthread_detach`, `pthread_mutexattr_setprotocol`, `pthread_rwlock_init` called on boot). Most
   map onto what the host already does for the `scePthread*`/`sceKernel*` names. The socket half
   (19) can wait and return errors.
5. **libSceSysmodule** (6 titles): `sceSysmoduleLoadModuleInternalWithArg` (called by Ayesha and
   all Unity) and `sceSysmoduleGetModuleInfoForUnwind` (C++ exception unwinding across modules).
6. **libSceDiscMap** (6 titles): `Func_7C980FFB0AA27E7A` called by Ayesha and all Unity at boot,
   `sceDiscMapIsRequestOnHDD`. For the called one shadPS4 returns `ORBIS_OK` but zeroes its three
   out-params (`flags`, `ret1`, `ret2`); the permissive return 0 leaves them as garbage, which the
   game then reads. The other DiscMap functions return `ORBIS_DISC_MAP_ERROR_NO_BITMAP_INFO` there.
7. **libSceSystemService stubs** (6 titles): `DisableMusicPlayer/ReenableMusicPlayer` (all six),
   `ParamGetString` (Ayesha, called), `GetHdrToneMapLuminance` (Unity, called) and 8 more Unity
   ones (suspend dialog, eye distance, power tick, abnormal termination). shadPS4 stubs them all;
   correct values for the getters.
8. **Gust boot inits** (2 titles, all called): libSceScreenShot (4),
   `sceGameLiveStreamingInitialize`, `sceSharePlayInitialize`, `sceAudiodecInitLibrary`, `sceNpTusCreateNpTitleCtxA`
   (Ryza). Trivial success stubs except Audiodec.
9. **libSceUlt HLE** (2 titles; `sceUltInitialize` is Ryza's first unsupported call without the
   dump): 7 functions, today only through the user's sprx; no shadPS4 reference, so an own
   implementation over the fiber/thread code (the sys-module-then-rewrite plan).
10. **libSceSaveData + libSceAppContent** (5 titles; Ayesha calls `sceSaveDataClearProgress`):
    backup/restore/progress/memory2 and TemporaryDataMount2/AddcontMount. Complete in shadPS4.
11. **libSceAudio3d** (6 titles import 7-16 functions; never called within 40 s) and
    **libSceAudiodec** decoding (Gust): next audio work once the games get further.
12. **Return-an-error stubs for absent hardware and services** (cheap, bulk count, low value;
    the NP part is the biggest block by count: libSceNpTus 41, libSceNpMatching2 18-25,
    libSceNpManager 11-18, libSceNpScore/NpWebApi 10-14 per NpToolkit2 title, and
    `sceNpTrophyRegisterContext`/`DestroyContext`/`DestroyHandle`/`AbortHandle` in all six
    non-Bloodborne titles; none called within 40 s; the bbnet online module covers most of it
    where it is built):
    libSceMove (`sceMoveInit` is called by Symphonia/Astrea), libSceHmd/HmdSetupDialog/VrTracker/
    Camera/Remoteplay/SocialScreen(Dialog)/Ime (Unity), the NP family (NpManager InGameMessage,
    NpUtility, NpWebApi, InvitationDialog, SigninDialog, WebBrowserDialog, ErrorDialog), Http/Ssl.

## Engine groupings

- **Unity IL2CPP (Symphonia, Astrea, SaGa, ALTDEUS)**: identical player eboot imports; 420 gaps
  shared by all four, 470 in their union. Shared by library: libSceNpTus 41, libkernel 40,
  libScePosix 36, libSceNpMatching2 25, libSceHmd 22, libSceHttp 22, libSceNpManager 18,
  libSceVrTracker 15, libSceNpScore 14, libSceSaveData 14, libSceNpWebApi 14, libSceNpTrophy 11,
  libSceNpUtility 10, libSceSystemService 10, libSceSsl 9, libSceCamera 8, libSceMove 7, libSceNet 7,
  libSceSocialScreen 7, libSceAppContent 7, libSceAudio3d 7, then dialogs and smaller ones. Per-title
  differences are in the plugins: SaGa has no fmod/NpToolkit plugin (libsead instead), ALTDEUS
  uses CRI (`cri_ware_unity.prx`) and adds libSceVideodec2 (9) and libSceScreenShot (3);
  Symphonia/Astrea add libSceErrorDialog and libSceFont's `sceFontGraphicsReportError`.
- **Gust (Ryza, Ayesha)**: 82 shared gaps (listed above). Ryza additionally imports the NP stack
  through the bundled NpToolkit2 (NpTus, NpSns, InvitationDialog, SigninDialog,
  GameCustomDataDialog, PlayGo); Ayesha is the leaner PhyreEngine build.
- **Common to all six non-Bloodborne titles** (47 NIDs): the libkernel boot set of item 1,
  libSceSysmodule's two, libSceDiscMap `Func_7C980FFB0AA27E7A`, SystemService
  Disable/ReenableMusicPlayer, libSceAudio3d's core six (Initialize, PortOpen/Close,
  ObjectReserve/SetAttributes, Terminate), `sceAudioOutSetMixLevelPadSpk`, WebBrowserDialog's four,
  NpTrophy's four context/handle functions, NpManager's four request/availability functions
  and `sceSaveDataDialogProgressBarSetValue`.

## Caveats

- Unity `Media/` statuses come from the dynmod branch build, whose host set is master's plus the
  module loading functions; on master itself those modules are never linked (the master runs call
  `sceKernelLoadStartModule`, then the Il2Cpp exports through unresolved pointers, and fault).
- Called markers cover only 40 s from boot with no input (Unity master runs fault within seconds);
  anything first called later in a session (menus, saving, gameplay) is not marked.
- The shadPS4 stub detection is textual (function body containing `(STUBBED)`), so a few partial
  implementations count as real and a few wrappers as stubs.

## Per library

Cell: `N` imports without implementation, `Ns` answered by a stub, `NL` covered only by the user's sys module (LLE); `ok` imported and fully implemented (host or a game module); `·` not imported. **!** a gap of that library was called in the master run, *!* called only in the dynmod-branch run. shadPS4: LIB_FUNCTION registrations / of which the body logs `(STUBBED)`. gaps in shadPS4: how many of all titles' gap NIDs in that library shadPS4 registers (real/stub/absent). sprx: the user's dump has it.

| Library | Titles w/ gaps | BB | Ryza | Ayesha | Symph | Astrea | SaGa | ALTD | shadPS4 reg/stub | gaps in shadPS4 (real/stub/none) | sprx |
|---|---|---|---|---|---|---|---|---|---|---|---|
| libkernel | 7 | 9 | 24**!** | 24**!** | 41**!** | 41**!** | 42**!** | 40**!** | 512/16 | 34/2/9 |  |
| libSceNpManager | 7 | 8s | 11 5s | 2 2s | 12 6s | 12 6s | 12 6s | 12 6s | 49/4 | 11/2/7 |  |
| libSceNpTrophy | 7 | 1s | 10 1s | 3 1s | 10 1s | 10 1s | 10 1s | 10 1s | 87/75 | 7/4/0 |  |
| libSceHttp | 6 | 14s | 3 4s | · | 13 9s | 13 9s | 13 9s | 13 9s | 115/49 | 22/6/0 |  |
| libSceNpMatching2 | 6 | 9s | 18 7s | · | 18 7s | 18 7s | 18 7s | 18 7s | 58/0 | 27/0/0 |  |
| libSceAudio3d | 6 | · | 15 | 15 | 16 | 16 | 7 | 14 | 38/16 | 18/0/0 |  |
| libSceNpWebApi | 6 | 6s | 10 4s | · | 10 4s | 10 4s | 10 4s | 10 4s | 100/49 | 15/1/0 |  |
| libSceNpScore | 6 | 4s | 10 4s | · | 10 4s | 10 4s | 10 4s | 10 4s | 51/12 | 10/4/0 |  |
| libSceNpUtility | 6 | 3s | 10 | · | 10 | 10 | 10 | 10 | 98/75 | 9/4/0 |  |
| libSceSystemService | 6 | ok | 2**!** | 3**!** | 12*!* | 12*!* | 12 | 10 | 80/74 | 1/11/0 |  |
| libSceSsl | 6 | 1s | 1 1s | · | 9 1s | 9 1s | 9 1s | 8 1s | 219/218 | 2/8/0 |  |
| libSceWebBrowserDialog | 6 | · | 4 | 4 | 8 | 8 | 4 | 4 | 13/9 | 3/5/0 |  |
| libSceNet | 6 | 5s | 1 1s | · | 2 5s | 2 5s | 2 5s | 2 5s | 209/144 | 7/0/0 |  |
| libSceNpAuth | 6 | 1s | 4 1s | · | 4 1s | 4 1s | 4 1s | 4 1s | 13/1 | 5/0/0 |  |
| libSceSaveDataDialog | 6 | ok | 1s | 1s | 2s | 2s | 2s | 2s | 10/0 | 2/0/0 |  |
| libSceSysmodule | 6 | ok | 2 | 2**!** | 2**!** | 2**!** | 2**!** | 2**!** | 16/8 | 2/0/0 |  |
| libSceUserService | 6 | 1s | 1s | ok | 1 1s | 1 1s | 1 1s | 1 1s | 421/415 | 1/1/0 |  |
| libSceAudioOut | 6 | ok | 1 | 1 | 1 | 1 | 1 | 1 | 66/59 | 0/1/0 |  |
| libSceDiscMap | 6 | ok | 1 | 1**!** | 1**!** | 1**!** | 1**!** | 1**!** | 5/0 | 1/0/0 |  |
| libSceNpTus | 5 | · | 41**!** | · | 41 | 41 | 41 | 41 | 142/6 | 41/0/0 |  |
| libSceSaveData | 5 | ok | ok | 4**!** | 14 | 14 | 15 | 14 | 95/66 | 15/0/0 |  |
| libSceAppContent | 5 | ok | 2 | · | 7 | 7 | 7 | 7 | 31/24 | 2/5/0 |  |
| libSceInvitationDialog | 5 | · | 5 | · | 5 | 5 | 5 | 5 | 9/0 | 5/0/0 |  |
| libSceSigninDialog | 5 | · | 5 | · | 5 | 5 | 5 | 5 | 7/7 | 0/5/0 |  |
| libSceNetCtl | 5 | 2s | ok | · | 1 | 1 | 1 | 1 | 53/49 | 0/3/0 |  |
| libSceNetCtlForNpToolkit | 5 | · | 3 | · | 3 | 3 | 3 | 3 | 4/3 | 1/2/0 |  |
| libSceNpCommerce | 5 | ok | 3 | · | 3 | 3 | 3 | 3 | 11/0 | 3/0/0 |  |
| libSceNpManagerForToolkit | 5 | · | 3 | · | 3 | 3 | 3 | 3 | 3/2 | 1/2/0 |  |
| libSceRazorCpu | 5 | · | 3 | · | 3 | 3 | 3 | 3 | 35/35 | 0/3/0 |  |
| libSceAjm | 5 | 1s | · | · | 1s | 1s | 1s | 1 1s | 22/9 | 0/2/0 |  |
| libSceMouse | 5 | 2s | · | · | 2s | 2s | 2s | 2s | 14/10 | 2/0/0 |  |
| libSceUserServiceForNpToolkit | 5 | · | 2 | · | 2 | 2 | 2 | 2 | 2/2 | 0/2/0 |  |
| libSceNpProfileDialog | 5 | ok | 1 | · | 1 | 1 | 1 | 1 | 8/1 | 0/1/0 |  |
| libSceRtc | 5 | ok | 1L | ok | 1L | 1L | 1L | 1L | 47/0 | 1/0/0 | yes |
| libScePosix | 4 | ok | ok | ok | 39*!* | 39*!* | 39*!* | 36*!* | 197/6 | 38/0/1 |  |
| libSceHmd | 4 | · | · | · | 22 | 22 | 22 | 22 | 178/157 | 11/11/0 |  |
| libSceVrTracker | 4 | · | · | · | 15 | 15 | 15 | 15 | 27/21 | 6/9/0 |  |
| libSceCamera | 4 | · | · | · | 8 | 8 | 8 | 8 | 76/6 | 8/0/0 |  |
| libSceMove | 4 | · | · | · | 7*!* | 7*!* | 7 | 7 | 11/0 | 7/0/0 |  |
| libSceSocialScreen | 4 | · | · | · | 7 | 7 | 7 | 7 | 0/0 | 0/0/7 |  |
| libSceHmdSetupDialog | 4 | · | · | · | 6 | 6 | 6 | 6 | 7/7 | 0/6/0 |  |
| libSceSocialScreenDialog | 4 | · | · | · | 6 | 6 | 6 | 6 | 0/0 | 0/0/6 |  |
| libSceErrorDialog | 4 | · | 5 | 5 | 4 | 4 | · | · | 8/2 | 5/0/0 |  |
| libSceIme | 4 | · | · | · | 5 | 5 | 5 | 5 | 54/43 | 4/1/0 |  |
| libScePad | 4 | ok | ok | ok | 4 | 4 | 5 | 4 | 92/78 | 3/2/0 |  |
| libSceRemoteplay | 4 | · | · | · | 5 | 5 | 5 | 5 | 42/41 | 1/4/0 |  |
| libSceMsgDialog | 4 | ok | 2 | 2 | 4 | 4 | ok | ok | 10/0 | 4/0/0 |  |
| libkernel_unity | 4 | · | · | · | 2*!* | 2*!* | 2*!* | 2*!* | 3/0 | 2/0/0 |  |
| libSceVideoOut | 4 | ok | ok | ok | 1 | 1 | 1 | 1 | 30/1 | 0/0/1 |  |
| libSceScreenShot | 3 | · | 4**!** | 4**!** | · | · | · | 3 | 13/13 | 0/5/0 |  |
| libSceAudioIn | 3 | ok | · | · | 2 | 2 | 1 | ok | 42/37 | 2/0/0 |  |
| libSceVideoRecording | 3 | · | 1 | 1 | · | · | · | 1 | 1/1 | 0/1/0 |  |
| libkernel_cpumode_platform | 3 | · | · | · | 1 | 1 | 1 | · | 1/0 | 1/0/0 |  |
| libSceUlt | 2 | · | 7L**!** | 6L | · | · | · | · | 0/0 | 0/0/7 | yes |
| libSceAudiodec | 2 | · | 6**!** | 6 | · | · | · | · | 0/0 | 0/0/6 |  |
| libSceGameLiveStreaming | 2 | · | 2**!** | 3**!** | · | · | · | · | 45/45 | 0/3/0 |  |
| libSceSharePlay | 2 | · | 2**!** | 3**!** | · | · | · | · | 24/24 | 0/3/0 |  |
| libSceFont | 2 | · | · | ok | 1 | 1 | · | · | 229/130 | 0/0/1 |  |
| libSceVideodec2 | 1 | · | · | · | · | · | · | 9 | 12/0 | 9/0/0 |  |
| libSceVoice | 1 | 8s | · | · | · | · | · | · | 30/30 | 0/8/0 |  |
| libSceGameCustomDataDialog | 1 | · | 5 | · | · | · | · | · | 0/0 | 0/0/5 |  |
| libSceNpSnsFacebookDialog | 1 | · | 5 | · | · | · | · | · | 1/1 | 0/1/4 |  |
| libSceNpSignaling | 1 | 4s | · | · | · | · | · | · | 24/0 | 4/0/0 |  |
| libSceNpSns | 1 | · | 3 | · | · | · | · | · | 0/0 | 0/0/3 |  |
| libScePlayGo | 1 | ok | 3 | · | · | · | · | · | 15/0 | 3/0/0 |  |

Imported and fully covered in every title that imports them: Il2CppUserAssemblies, PS4Util, libSceAvPlayer, libSceCommonDialog, libSceFace, libSceFios2, libSceFontFt, libSceGnmDriver, libSceGnmDriverResourceRegistration, libSceImeDialog, libSceLibcInternal, libSceLibcInternalExt, libSceNgs2, libSceNpCommon, libSceNpToolkit2, libc, libfmod, ulobjmgr.

## Called at runtime

First calls, in order, of imports without implementation (`unsupported import ... returns 0` lines; stubs are not logged when called). Master runs are 40 s; dynmod runs are the dynmod worktree build, which loads the Unity titles' Media/ modules and so gets further.

- **bb** master: none
  - with sys modules, also: sceKernelConvertUtcToLocaltime (sys module import)
- **ryza** master: sceUltInitialize (libSceUlt, sys_lle), sceKernelIsAddressSanitizerEnabled (libkernel), sceKernelIsNeoMode (libkernel), scePthreadCondattrInit (libkernel), scePthreadCondattrDestroy (libkernel), sceScreenShotDisable (libSceScreenShot), sceScreenShotSetParam (libSceScreenShot), sceScreenShotSetOverlayImage (libSceScreenShot), sceScreenShotEnable (libSceScreenShot), sceKernelConvertUtcToLocaltime (libkernel), sceSystemServiceDisableMusicPlayer (libSceSystemService), sceAudiodecInitLibrary (libSceAudiodec), sceNpTusCreateNpTitleCtxA (libSceNpTus), sceGameLiveStreamingInitialize (libSceGameLiveStreaming), sceSharePlayInitialize (libSceSharePlay)
  - with sys modules, also: sceKernelGetCompiledSdkVersion (sys module import)
- **ayesha** master: sceKernelIsAddressSanitizerEnabled (libkernel), sceKernelIsNeoMode (libkernel), sceSystemServiceParamGetString (libSceSystemService), sceScreenShotDisable (libSceScreenShot), sceScreenShotSetParam (libSceScreenShot), sceScreenShotSetOverlayImageWithOrigin (libSceScreenShot), sceScreenShotEnable (libSceScreenShot), sceGameLiveStreamingInitialize (libSceGameLiveStreaming), sceSharePlayInitialize (libSceSharePlay), sceSysmoduleLoadModuleInternalWithArg (libSceSysmodule), Func_7C980FFB0AA27E7A (libSceDiscMap), sceSystemServiceDisableMusicPlayer (libSceSystemService), sceSaveDataClearProgress (libSceSaveData), scePthreadCondattrInit (libkernel), scePthreadCondattrDestroy (libkernel), sceKernelConvertUtcToLocaltime (libkernel)
  - with sys modules, also: sceKernelGetCompiledSdkVersion (sys module import)
- **symphonia** master: sceKernelSetVirtualRangeName (libkernel), sceKernelDirectMemoryQuery (libkernel), sceKernelIsNeoMode (libkernel), sceSysmoduleLoadModuleInternalWithArg (libSceSysmodule), Func_7C980FFB0AA27E7A (libSceDiscMap), sceKernelLoadStartModule (libkernel), SetDataFolder (Il2CppUserAssemblies, game_module), il2cpp_api_register_symbols (Il2CppUserAssemblies, game_module), il2cpp_api_lookup_symbol (Il2CppUserAssemblies, game_module), unity_mono_set_user_malloc_mutex (Il2CppUserAssemblies, game_module)
  - dynmod: sceKernelSetVirtualRangeName (libkernel), sceKernelDirectMemoryQuery (libkernel), sceKernelIsNeoMode (libkernel), sceSysmoduleLoadModuleInternalWithArg (libSceSysmodule), Func_7C980FFB0AA27E7A (libSceDiscMap), sceKernelIsAddressSanitizerEnabled (libkernel), sceKernelGetCpumode (libkernel), pthread_setcancelstate (libScePosix), scePthreadCondattrInit (libkernel), scePthreadCondattrDestroy (libkernel), sceKernelInstallExceptionHandler (libkernel_unity), pthread_detach (libScePosix), scePthreadGetschedparam (libkernel), scePthreadSetschedparam (libkernel), sceSystemServiceGetHdrToneMapLuminance (libSceSystemService), sceMoveInit (libSceMove), pthread_mutexattr_setprotocol (libScePosix), sceKernelRaiseException (libkernel_unity)
- **astrea** master: sceKernelSetVirtualRangeName (libkernel), sceKernelDirectMemoryQuery (libkernel), sceKernelIsNeoMode (libkernel), sceSysmoduleLoadModuleInternalWithArg (libSceSysmodule), Func_7C980FFB0AA27E7A (libSceDiscMap), sceKernelLoadStartModule (libkernel), SetDataFolder (Il2CppUserAssemblies, game_module), il2cpp_api_register_symbols (Il2CppUserAssemblies, game_module), il2cpp_api_lookup_symbol (Il2CppUserAssemblies, game_module), unity_mono_set_user_malloc_mutex (Il2CppUserAssemblies, game_module)
  - dynmod: sceKernelSetVirtualRangeName (libkernel), sceKernelDirectMemoryQuery (libkernel), sceKernelIsNeoMode (libkernel), sceSysmoduleLoadModuleInternalWithArg (libSceSysmodule), Func_7C980FFB0AA27E7A (libSceDiscMap), sceKernelIsAddressSanitizerEnabled (libkernel), sceKernelGetCpumode (libkernel), pthread_setcancelstate (libScePosix), scePthreadCondattrInit (libkernel), scePthreadCondattrDestroy (libkernel), sceKernelInstallExceptionHandler (libkernel_unity), pthread_detach (libScePosix), scePthreadGetschedparam (libkernel), scePthreadSetschedparam (libkernel), sceSystemServiceGetHdrToneMapLuminance (libSceSystemService), sceMoveInit (libSceMove), pthread_mutexattr_setprotocol (libScePosix), sceKernelRaiseException (libkernel_unity)
- **saga** master: sceKernelSetVirtualRangeName (libkernel), sceKernelDirectMemoryQuery (libkernel), sceKernelIsNeoMode (libkernel), sceSysmoduleLoadModuleInternalWithArg (libSceSysmodule), Func_7C980FFB0AA27E7A (libSceDiscMap), sceKernelLoadStartModule (libkernel), SetDataFolder (Il2CppUserAssemblies, game_module), il2cpp_api_register_symbols (Il2CppUserAssemblies, game_module), il2cpp_api_lookup_symbol (Il2CppUserAssemblies, game_module), unity_mono_set_user_malloc_mutex (Il2CppUserAssemblies, game_module)
  - dynmod: sceKernelSetVirtualRangeName (libkernel), sceKernelDirectMemoryQuery (libkernel), sceKernelIsNeoMode (libkernel), sceSysmoduleLoadModuleInternalWithArg (libSceSysmodule), Func_7C980FFB0AA27E7A (libSceDiscMap), sceKernelIsAddressSanitizerEnabled (libkernel), sceKernelGetCpumode (libkernel), pthread_setcancelstate (libScePosix), scePthreadCondattrInit (libkernel), scePthreadCondattrDestroy (libkernel), sceKernelInstallExceptionHandler (libkernel_unity), pthread_detach (libScePosix), sceKernelRaiseException (libkernel_unity)
- **altdeus** master: sceKernelSetVirtualRangeName (libkernel), sceKernelIsNeoMode (libkernel), sceKernelDirectMemoryQuery (libkernel), sceSysmoduleLoadModuleInternalWithArg (libSceSysmodule), Func_7C980FFB0AA27E7A (libSceDiscMap), sceKernelLoadStartModule (libkernel), SetDataFolder (Il2CppUserAssemblies, game_module), il2cpp_api_register_symbols (Il2CppUserAssemblies, game_module), il2cpp_api_lookup_symbol (Il2CppUserAssemblies, game_module), unity_mono_set_user_malloc_mutex (Il2CppUserAssemblies, game_module)
  - dynmod: sceKernelSetVirtualRangeName (libkernel), sceKernelIsNeoMode (libkernel), sceKernelDirectMemoryQuery (libkernel), sceSysmoduleLoadModuleInternalWithArg (libSceSysmodule), Func_7C980FFB0AA27E7A (libSceDiscMap), pthread_rwlock_init (libScePosix), sceKernelIsAddressSanitizerEnabled (libkernel), pthread_setcancelstate (libScePosix), scePthreadCondattrInit (libkernel), scePthreadCondattrDestroy (libkernel), sceKernelInstallExceptionHandler (libkernel_unity), pthread_detach (libScePosix), sceKernelRaiseException (libkernel_unity)

## Totals per title

| Title | Imports (unique NIDs) | host | game module | sys LLE only | stub | missing | gaps shadPS4 real/stub/none |
|---|---|---|---|---|---|---|---|
| bb | 779 | 450 | 250 | 0 | 70 | 9 | 57/16/6 |
| ryza | 819 | 396 | 152 | 8 | 30 | 233 | 182/49/32 |
| ayesha | 592 | 329 | 170 | 6 | 4 | 83 | 56/18/13 |
| symphonia | 1865 | 499 | 914 | 1 | 44 | 407 | 317/104/30 |
| astrea | 1872 | 499 | 921 | 1 | 44 | 407 | 317/104/30 |
| saga | 1702 | 497 | 773 | 1 | 44 | 387 | 302/100/29 |
| altdeus | 1689 | 489 | 759 | 1 | 44 | 396 | 308/103/29 |

Gaps in the imports of the user's sys modules themselves (only with BB_SYS_MODULES): sceKernelGetCompiledSdkVersion: bb; sceKernelSettimeofday: bb, ryza, ayesha; sceKernelConvertUtcToLocaltime (called): bb; sceKernelConvertLocaltimeToUtc: bb; sceKernelError: bb, ryza, ayesha; clock_settime: bb, ryza, ayesha; pthread_equal: ryza, ayesha; sceKernelGetCompiledSdkVersion (called): ryza, ayesha; ulobjmgr Func_1D9F50D9CFB8054E: ryza, ayesha.

