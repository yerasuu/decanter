# Testing triage: who can close each `testing` task

Date: 2026-10-29. Source: `kb list doing parked --tag testing` (116 tasks) read with `kb show`. Read-only triage; nothing on the board was changed.

Counts per verdict: WBC3 23, WINDOWS 18, NIGHT 2, OWNER 59, NONE 14. Total 116.

Conventions: dates are comment dates on the task (2026-10-DD HH:MM). "env" lists are limited to the wbc3 `env_allow` names; where a task needs a variable that is not in that list it is a blocker ("needs a policy line"). Commits are psport unless stated. Verdicts are strict: a "Done when" that needs a human judgement, a controller, online play, a GOG or Steam account, audio feel, a console, or the owner's dumped system modules (`PS_SYS_MODULES`, not on wbc3 or Windows) is OWNER.

## WBC3 (agent-wbc3, RTX 2080, headless regress)

| id | task | tags | check | evidence on board | blockers |
|---|---|---|---|---|---|
| 1270 | recycle guest thread stacks and TLS | doing, psport kernel fix | `regress` Bloodborne 5 min on origin/develop >= fff1471a20; pass: exit summary shows stack/TLS recycle counters, no STOP, no crash. The Ryza low-memory exhaustion itself is OWNER (needs `PS_SYS_MODULES`). | 10-29 19:40 tests already requested on wbc3 | needs the owner's push done (origin already at fff1471a20) |
| 1269 | reuse guest thread records | doing, psport kernel fix | Same regress run: thread-record reuse counters and peak live threads in the exit summary, no "no low memory for a thread record" line. Ryza proper is OWNER. | none (built with -Werror only) | none |
| 1265 | guest thread creation log flood | doing, psport kernel fix | Same run: first 3 creations per name + 10 s summary + exit summary line in the Bloodborne log, no per-thread line flood. | none | none |
| 1224 | GC marks overflow with VRAM budget < 2 GB | parked, psport gpu fix | `regress` Bloodborne with `PS_VRAM_LIMIT_MB=512`: log line `Texture cache: VRAM budget` shows recover/pressure/critical marks <= budget (expected 0/512/512 MiB) and GC collects. | 10-29 13:44 hand-computed marks only | none |
| 1207 | native Ngs2 by default, drop ngs2_anyps5 | parked, psport audio refactor | Regress Ryza and Ayesha with no `PS_NGS2`: log selects native Ngs2, 0 STOP, 0 unimplemented, audio peak > silence. Build/licences part already done. | 10-29 11:39 (build, 34 files, licences) | audio feel stays OWNER |
| 1183 | driver pipeline cache, VRR, FPS limit | parked, psport perf fix | Bloodborne twice with the SAME user folder (`workdir` profile), `PS_DRIVER_PIPELINE_CACHE` unset then `=0`: second run loads `driver.vkcache` (no cold compiles), perf unchanged. VRR (`PS_VRR=1`) is OWNER (needs a VRR display). | 10-29 11:32 n1031: cache created/saved, no perf regression; reuse NOT tested | none |
| 1182 | frame generation quality on NVIDIA | parked, psport gpu fix | `PS_FRAME_GEN=1` + `PS_VK_VALIDATION=1` + `PS_PRESENT_MODE=fifo` on Bloodborne, frame dumps of consecutive generated frames: luma of black frames equal to the real frames (no lifted blacks), 0 VUID. | 10-29 11:26 j183: 0 VUID, 128 generated/127 presented; flicker "pending a capture in motion" | visual judgement of flicker may need the owner |
| 1218 | Saga sig11 on RTX 2080 | doing, psport saga kernel fix | Saga CUSA43027 script x6 on origin/develop >= 843a6100d7 (includes layer ordering fix 189f9b1437); pass: 6/6 complete the schedule (~131 s), no `Guest fault`, no `heap error is detected`. AMD side: one regress pass equal to the base (NIGHT). | 10-29 18:57 j267 running (Saga x6, then Ryza/bo2, 14 titles); 10-29 18:14 pre-fix still failing | wait for j267/j268/j279 results |
| 624 | native Audiodec | parked, audio refactor | Regress Ryza: two M4AAC decoders without `not decodable` or `FFmpeg rejected`; Ayesha too. Sound equality is OWNER. | 10-28 15:12 integrated, night check pending | none |
| 831 | no assertions: AvPlayer, VideoDec | parked, video fix | Regress of the video titles: 0 `Assertion`, 0 STOP, calls report lists misuse as counted errors. | 10-29 11:32 (n1031 on #1183: 0 asserts, 0 STOP, 13 titles) shows the regress side already clean | could close with that evidence |
| 592 | AvPlayer indexes streams twice | parked, system video fix | Regress of titles with video (symphonia, ryza): decodes, no new errors; compare with base. | 10-24 13:56 integrated; n1031 regress equal | none |
| 593 | VideoOut `Out of order flip IRQ` abort | parked, video fix | Regress all titles: no `Out of order flip IRQ` abort, counted warning line only. | 10-24 13:56; n1031 0 STOP | none |
| 595 | stale ImageId in texture cache | parked, gpu fix | Regress + `PS_VK_VALIDATION=1` Bloodborne in game with upscaler on: 0 VUID, 0 stale-id errors. | 10-24 13:56; n1028 in game no crash | none |
| 477 | GPU memory arenas above maxBufferSize | parked, gpu vendor fix | Regress on RTX 2080 with `PS_VK_VALIDATION=1`: 0 VUID incl. 06409, line `Sparse arena of N b...` present, no null bindings. Second vendor check. | 10-24 15:13 n1028 (AMD): 0 VUID in game | none |
| 575 | Blood Omen 2 compute hang | parked, gpu bloodomen2 fix | Regress bo2 (`bloodomen2`) 2 runs: no GPU hang, reaches the schedule end. | 10-24 13:56 integrated, no run result | needs bo2 in the 14 titles (it is, per j267 "Ryza/bo2") |
| 370 | P7 shaders compiled ahead | parked, gpu perf | Bloodborne twice, same user folder: 0 in-game compiles on warm cache, worst frame lower (`PS_FRAME_STATS=1`). | 10-18 12:59 only | P7 toggle variable not in env_allow (needs a policy line) |
| 796 | preload with cross motion metadata | parked, gpu fix | Same run as #370 with `PS_PRELOAD_MOTION_SHADERS` 0 and 1; pass: `graphics pipelines not preloaded` line present and 0. | 10-24 07:52 inconclusive | needs #370; variable not in env_allow |
| 504 | Bloodborne crash at 0x2199310, GX heap | parked, gpu bloodborne fix | Seeded Bloodborne in-game run: `LogGxNodeNull` lines appear or not. If nodes null appear, the state dump gives the cause. | 10-22 08:43 run stayed in the menu | pass/fail depends on the fault occurring; may stay unproven |
| 750 | SRT walker null pointers | parked, gpu bloodborne fix | Same seeded in-game run: `PsSrtFaults` lines with shader hash and table path, then fix. | 10-22 08:43 no faults seen | same caveat |
| 889 | Octopath 0 pad schedule reaching play | parked, regress-test | New pad schedule iterations on Octopath 0 until the 3D scene is reached with presentation dumps. | 10-24 13:57: 12 sonnet passes, goal NOT met | needs a schedule that passes the loading screen; likely low yield |
| 787 | depth of field follows live resolution | parked, gpu bloodborne fix | Bloodborne with `PS_SCALE_REDUCED_TARGETS=1` at 1440p, frame dumps compare DoF sharpness. | 10-24 n1028: 225 FPS, no crash, sharpness not compared | variable not in env_allow; sharpness may need the owner's eye |
| 368 | P5 native resource binding | parked, gpu feat | Regress + capture + perf with `PS_SAMPLER_HEAP=1` vs off; bo2 passes the recompiler. | 10-18 13:20 only | variable not in env_allow (policy line) |
| 365 | P2 native sync | parked, gpu feat | Regress + PM4 autotest 11/11 with `PS_LABEL_WAITS=1`. | 10-18 13:03 only | variable not in env_allow (policy line) |

## WINDOWS (Windows PC, RTX 4060 Laptop + Intel, Bloodborne dump only)

| id | task | tags | check | evidence | blockers |
|---|---|---|---|---|---|
| 1145 | Windows red zone guard pass | parked, psport windows loader | W1/W2: build psport at origin/develop, `decanter ps run` Bloodborne with `PS_RED_ZONE_GUARD=1` 5 min: log shows patched sites, plays clean; repeat with guard 0. L3 done on Linux at night. | 10-29 03:55 Linux: 14891 sites patched, 222.8 FPS, no faults | no scenario exercises the guard yet (suggested: Bloodborne 1.09 particle leaves) |
| 968 | Windows `mprotect failed` for FPS hooks | parked, psport windows bloodborne | Build psport >= 905a8a44df, run Bloodborne: no `no memory next to the game's code` and no `mprotect failed`; FPS limit of patches applied. | 10-24 23:05 error 1 gone at f8924bf8cb; 10-29 09:45 repeat with 905a8a44df requested | none |
| 966 | CI Windows package carries `patches/` | parked, ps windows toolchain fix | Download the next release's Windows package, decanter accepts it, Bloodborne passes patch load. | 10-24 22:07 integrated | needs a new psport release tag (owner) |
| 959 | AnyPS5 symlink on Windows | parked, ps windows fix | Build decanter on Windows (compiles) and Linux unchanged; AnyPS5 run not possible. | 10-24 21:30 j25/j27: not testable, no AnyPS5 on the PC | only the compile part is checkable |
| 899 | Windows test package and diagnostic report | parked, psport windows toolchain feat | Run the CI zip on Windows with the probe: report file generated, verdict WOULD WORK (native NVIDIA/Intel said memory import works). | 10-24 16:18 under Proton verdict WOULD NOT WORK; native 10-24 16:47 import would work | needs a CI artifact of a recent commit |
| 902 | decanter PS section on Windows | parked, ps windows app feat | Build decanter at develop, launch GUI: PS link visible with Testing badge, direct launch with a psport.exe folder works. | 10-24 11:42 integrated | none |
| 614 | psport win32 build | parked, psport windows feat | Build and run psport.exe Bloodborne on Windows: window, swapchain, first frames, no crash. | 10-24 16:47 native: import would work; no game yet | none |
| 801 | keyboard-as-stick settings in PS section | parked, ps feat | GUI screenshot of the PS section with a psport that lists `key_stick`: group Teclado with 2 rows (`key_stick_opposite`, `key_stick_hold_ms` 0-200 step 10). | 10-22 00:58 integrated; hidden until psport lists it (#785 integrated 10-22) | none |
| 526 | Job Object for the game session | parked, runtime windows feat | Launch a game that spawns a launcher exiting early, graceful stop and forced stop: whole tree gone (`tasklist`/`Get-Process`). | 10-29 13:43 retaken: only the Windows test left | needs a game with launcher chain (Bloodborne psport.exe is a single process) |
| 755 | hide PS options the machine cannot run | parked, ps gpu feat | GUI screenshots: on the NVIDIA GPU DLSS is offered, on Intel the Intel tonemap row shows; compare with this machine (owner). | 10-21 16:34 integrated | both GPUs are on the same PC, GPU selection is manual |
| 754 | missing psport options in PS section | parked, ps feat | Screenshots: DLSS, pad motion, toy pad, anisotropic, Intel tonemap, system fonts rows present in Spanish and reach psport at launch (`ps run` env echo in log). | 10-21 14:49 | Spanish reading is a light owner check |
| 753 | new psport settings, defaults review | parked, ps feat | Screenshots of group Desarrollo (keep_logs, camera_cuts, image_traps) and defaults. | 10-21 14:17 | none |
| 566 | Windows integrated frame and no testing badge | parked, app windows fix | Screenshot of the title bar on Windows 11 at v0.3.3 or later: integrated frame, no "en pruebas" badge. | 10-22 11:28 v0.3.3 published | none |
| 440 | decanter on Windows and macOS phase 1 | parked, windows macos | Windows items from the 10-22 09:25 list: tray, close to tray, Ctrl+Shift+F4 stop after 5 s, startup settings. | 10-24 17:40 j1 steps 2-5 passed (folder accepted/rejected with reasons) | tray click needs a pointer (OWNER) and macOS is OWNER; keep parked after the Windows part |
| 432 | remove green dots in sidebars | parked, app style | Screenshot of the Library and PS sidebars: no green dots, runnable vs running distinguishable. | 10-16 15:10 integrated | none |
| 982 | collect error reports | parked, app ps cli feat | `decanter ps report` on Windows and on this side with the same extractors: report has failures, logs and system data, no personal data. | 10-28 10:44 phases 1 and 2 integrated; visual check by the owner | Windows side needs a recent Bloodborne log |
| 1036 | closed vocabulary and forbidden list for reports | parked, app ps feat | `decanter ps report` over logs containing a one-word user name or PSN id in a message: not emitted, unknown message gives type only. | 10-25 21:34 integrated, SCHEMA 2 | needs crafted log lines (agent-written) |
| 1271 | split metrics from log in PS job view | doing, app ps feat | GUI screenshots of a Bloodborne job: tabs Registro / Rendimiento, copy and full log still give everything. | 10-29 19:40 integrated, UI untested | the owner's GUI check is the fallback |

## NIGHT (this machine, AMD, unattended)

| id | task | tags | check | evidence | blockers |
|---|---|---|---|---|---|
| 573 | buffer cache loses ~60 % FPS with low VRAM | parked, gpu perf | Bloodborne perf run with `PS_VRAM_LIMIT_MB` at 1500 and 2200 MiB vs unlimited; pass: FPS drop <= 10 %, `resume` line seen. | 10-18 13:11 cause in n1018 logs (no `resume`) | none; combine with #1224 marks fix |
| 762 | intermittent guest crash 0x263b8e7 | parked, bloodborne kernel fix | Keep counting Bloodborne runs on develop; pass: 0 crashes over a further N runs, then close as unreproducible. | 10-24 07:52: 0 of 445 runs crashed | "Done when" (commit identified with tests both sides) is not reachable |

## OWNER

| id | task | tags | check | evidence | blockers |
|---|---|---|---|---|---|
| 1268 | Ryza water not drawn | doing, psport gpu ryza | Play Ryza in the stone canal (AM 10) with `PS_SYS_MODULES`, water surface visible; read the live-depth diagnostic log. | 10-29 19:37 water still not drawn, diagnostics added at 56355d8d30 | open root cause |
| 1266 | LncUtil, SharePlay, EINVAL sema/eventflag, PlayGo | doing, psport system fix | Ryza with `PS_SYS_MODULES`: calls report shows no EINVAL for CreateSema/CreateEventFlag, no unimplemented. | 10-29 18:06 integrated, no run | needs the dumped modules |
| 1263 | AppContent load counted as native | doing, psport system fix | Ryza with `PS_SYS_MODULES`: no `STOP AppContent called before module load`. | 10-29 17:52 owner log: Ryza running and playing, STOP gone | close after the owner confirms |
| 1255 | sysmodule load of unlinked modules | doing, psport kernel fix | Ryza/Ayesha with `PS_SYS_MODULES`: main() passes. | 10-29 17:32 owner: Ryza passes main and assert | needs the dumped modules; candidate to close |
| 1254 | missing kernel calls, appinfo sysctl, offline net, dipsw, video/avcap (#1248 A,C,D) | doing, psport kernel system | Ryza/Ayesha `PS_SYS_MODULES` calls report: those calls no longer unimplemented. | 10-29 16:45 integrated | needs the dumped modules |
| 1253 | native NpCommon, Np heap, operator new | doing, psport kernel system | Same run: 14 exports resolve, no STOP. | 10-29 16:36 integrated, ASan harness | same |
| 1252 | native shm_open, NEEDED order, no libSceHmd | doing, psport kernel system | Same run: Ryza starts past libSceHmd. | 10-29 16:41 integrated | same |
| 1228 | GOG clean room step 2 | parked, gog app refactor | Login, token renewal, library list and achievements by hand on a GOG account. | 10-29 13:55 integrated, compiles | needs a GOG account |
| 1186 | BBSC packets 0x23-0x27, shadNet keepalive | parked, psport net fix | Boss wipe with two players (BBSC life packet 0x23), short network drop. | 10-29 11:37 question posted | online only (the owner) |
| 1185 | phantom mouse from Steam Desktop Layout/reWASD | parked, psport input fix | On the Steam Deck: right stick with captured mouse, camera at stick speed only, log line `Mouse: motion from mouse`. | 10-29 10:50 | Steam Deck |
| 1165 | hot toggle of frame generation, menu read-back | parked, psport gpu feat | In game: toggle in the menu, decanter shows the change; no pointer automation possible. | 10-29 03:02 (cause of the 66 % without capture found) | needs the menu |
| 684 | Yebis nBaseSamples = 2 | parked, bloodborne gpu fix | Play through areas with gparam 3 with `PS_QUIRKS=-bloodborne_blur_sample_clamp`. | 10-29 10:39 n1030 inconclusive, quirk stays | needs gameplay areas |
| 939 | win32-bundle.sh finds dependency licences | parked, psport windows ci | Check the next push's Windows CI is green (main session can `gh run list`). | 10-24 18:59 b4751372e | needs the owner's push |
| 926 | publish Windows toolchain by tag | parked, psport windows ci | After tag `toolchain-win32-N` a test build with the pinned llvm-mingw gives a psport.exe identical in behaviour. | 10-24 18:32 integrated | needs the owner's tag/push |
| 920 | attach Windows package and .sha256 to release | parked, ci | After the next version tag: release carries zip and `.sha256`. | 10-24 17:08 integrated | needs a version tag |
| 740 | stutters with online messages | parked, bloodborne gpu perf | Open and rate online messages in Bloodborne, no > 50 ms frames with write faults in the log. | 10-24 15:13 harness cannot measure it | online + hands |
| 683 | job-runner step failures, Laurence cinematic | parked, bloodborne gpu fix | `PS_JOB_STEP_PROBE=1` at the Laurence cutscene. | 10-24 15:13 no cutscene in schedule | gameplay |
| 685 | PM4/23 on load with FX init cleanups | parked, bloodborne gpu fix | Load a save; `PS_GPU_WRITE_OVERLAP=1` is too costly (20 FPS). | 10-24 15:13 | needs a fix design |
| 348 | Bloodborne sound flag general fix | parked, system bloodborne | Decision: the quirk is still needed (peak -304 dBFS without it). | 10-24 07:52 | needs rea analysis, not a test |
| 782 | keyboard glyphs per game | parked, input feat | Review Ayesha capture of the 20 glyphs; Ryza and rest still to annotate. | 10-22 08:47 | visual |
| 598 | play time at high FPS | parked, bloodborne loader | Long session: in-game play time vs wall clock. | 10-22 08:47 | long hands-on session |
| 596 | gamepad on-screen keyboard for IME | parked, system input | Use the OSK with a controller in the IME dialog. | 10-22 08:47 | controller |
| 794 | Bloodborne online out of generic gpu/net | parked, net bloodborne refactor | Bloodborne online same as before. | 10-22 08:47 | online |
| 785 | input, IME, overlay of Supermedo fork | parked, input system fix | Pad and real window tests. | 10-22 08:47 | controller, window |
| 395 | Ngs2 LFE 120 Hz, limiter 0x30000003 | parked, audio fix | Run `tools/hwtest/ngs2_limiter` on a PS4. | 10-22 08:47 | nobody has a console; consider closing by decision |
| 786 | Supermedo patches, mods, stereo mix | parked, loader audio fix | Hear the stereo mix on a 2-channel device; test mods. | 10-22 08:47 | audio, hands |
| 739 | EpollWait EINTR as status in report | parked, psport net fix | Online Bloodborne call report lists EINTR under status. | 10-22 08:47 | online |
| 758 | GOG list error message | parked, gog fix | Library lists again; an expired session gives an explained error. | 10-21 19:22 | GOG account |
| 749 | upscaler history reset on camera cuts | parked, gpu bloodborne fix | Bloodborne cutscenes without flashes; log `Camera motion: cut at`. | 10-21 13:02 | visual |
| 734 | extensible i18n | parked, app i18n refactor | Switch language: all texts change incl. shown notices; add a language by files. | 10-21 11:23 | reading Spanish |
| 731 | sceNetEpollAbort | parked, net fix | Online Bloodborne unchanged. | 10-21 11:12 stub gone, EINTR flood | online |
| 728 | BO1/BO2 crosshair with keyboard and mouse | parked, bo1 bo2 input | Keyboard on: crosshair on; pad: off. | 10-22 00:16 | hands, BO titles |
| 725 | BO1/BO2 native mouse camera | parked, input | Mouse look feel. | 10-22 00:16 | hands |
| 722 | BO1/BO2 shooter default controls | parked, input | `psport --list-controls --title CUSA57547/CUSA57548`; mouse feel. | 10-22 00:16 | feel |
| 643 | BBSC kit with empty chest | parked, net bloodborne fix | New character receives the kit, failed copy blocks delivery. | 10-22 00:16 | online |
| 642 | BBSC reads guest memory by psport map | parked, net bloodborne fix | BBSC leaves the kit ready. | 10-22 00:16 | online |
| 653 | bbport 0.6 settings and launcher | parked, psport fix | Launcher and focus capture on this machine. | 10-22 00:16 | hands |
| 626 | antispam.yml fix | parked, ci | Needs a real GitHub comment event. | 10-19 09:08 | GitHub |
| 622 | debug mode with chosen log folder | parked, app ps feat | Enable, restart, play a PS and a PC game, see `<folder>/<host>/`. | 10-18 19:47 | needs a PC game (GOG) |
| 127 | prebuilt psport builds in CI | parked, toolchain ps ci | Start a game with the cached build from a clean checkout. | 10-18 18:25 CI green; lookup finds the build, nix copy 1.3 s | one game launch on this machine |
| 279 | loose `run.sh` finds Vulkan drivers | parked, psport toolchain fix | `PS_GAME_DIR=<game> bash run.sh` from a normal terminal opens the window. | 10-18 17:41 Vulkan check without a game OK | needs this desktop |
| 473 | Vulkan validation layers toggle without the layer | parked, ps fix | Enable the toggle: on-demand nix-build, warning on failure. | 10-18 17:34 | needs nix and network |
| 550 | real gyro and accelerometer | parked, input feat | Use a real pad. | 10-17 14:31 | controller |
| 551 | LEGO toy pad (USB) | parked, system feat | `PS_TOYPAD=1` with a game. | 10-17 14:28 | game and hands |
| 548 | bbport input improvements | parked, input feat | Pad, keyboard, mouse behaviours. | 10-17 14:13 | controller |
| 515 | DirectX 7 hint on Windows | parked, app windows feat | Needs a GOG native game that fails. | 10-17 12:15 | GOG game on Windows |
| 528 | Windows Galaxy DLL proxy | parked, achievements windows feat | A GOG game on Windows registers achievements. | 10-17 12:15 | GOG game |
| 527 | run external installer natively (Windows) | parked, app windows feat | An external game installer exe/msi runs. | 10-17 12:15 | an installer |
| 513 | launch native games without Proton (Windows) | parked, runtime windows feat | A native Windows game starts and stops. | 10-17 12:15 | GOG game |
| 503 | seamless co-op BBSC | parked, bloodborne net feat | In game co-op. | 10-17 12:05 | online |
| 501 | shadNet client, Seamless Hunt servers | parked, net feat | Online. | 10-17 12:05 | online |
| 431 | rollback to last port commit that built | parked, ps feat | Force a failing port update; see the failed commit and retry. | 10-16 15:10 | needs a breaking port commit |
| 406 | dates and rating in game page | parked, app fix | Look at a game page with achievements. | 10-16 15:10 | GOG library |
| 390 | tool update detection and self-update | parked, app ps toolchain feat | Check the button, detect and apply a new GitHub release. | 10-16 17:05 | network |
| 460 | GameMode per game and global | parked, runtime ps linux | `gamemoded -s` shows the game. | 10-16 16:33 | this machine |
| 459 | MangoHud per game and global | parked, runtime ps linux | Overlay appears for a Proton and a PS game. | 10-17 12:06 | this machine |
| 461 | psport metrics in the Registro | parked, ps app feat | After a PS game with metrics, the Registro shows the summary and a small chart. | 10-17 12:06; see also #1271 | GUI |
| 420 | relocatable psport bundle without nix | parked, toolchain linux | decanter runs a PS4 game with the bundle on a machine without nix. | 10-17 04:26 bundle ran Bloodborne without nix at 189.5 FPS; decanter route not tried | needs decanter Release channel |
| 865 | native GOG on macOS | parked | Install and play a native macOS GOG game. | 10-24 11:27 integrated | no macOS machine |

## NONE (close now or already decided)

| id | task | evidence |
|---|---|---|
| 1035 | report validator shape fixes | 10-25 21:29: 35/35 battery cases rejected, 2 references accepted, 111 real-log reports valid, 0 private strings, 402 field cases accepted before vs 105 after. Done when proven. |
| 992 | closed schema and allow-list normalizer | same 10-25 21:29 comment on #1035 (real logs: 111 valid, 0 private strings; battery 35/35 rejected). |
| 1039 | report server H1-H3 | 10-26 16:58: fmt, clippy, 47 tests, probes before/after (POST from 2140 ms to 7-18 ms at 99.9k rows, 40 GET parallel). |
| 1040 | report server M1-M4 and hardening | 10-26 17:11: 63 tests, `systemd-analyze security --offline` 0.9 SAFE (was 2.2). |
| 1048 | report server on s3db | 10-28 09:42: 72 tests, clippy clean, SQLite removed; MinIO start not exercised but Done-when probes covered locally. |
| 993 | report server (receive and public pages) | 10-25 23:14 built and verified, security review run, fixes #1039/#1040 landed. |
| 597 | FSR 3.1 frame generation | 10-29 01:54 n1029 (fg off 216.7 FPS vs on 160.7, startup line OK, no regression off) and 10-29 09:34 RTX 2080 (2x FPS shown 60 on a 30 game, 0 VUID). Cost is a product decision, not a test. |
| 805 | psport.exe links and starts under Proton | 10-23 07:49 integrated, starts; 10-23 08:17 reaches window, surface, swapchain. |
| 621 | psport debug mode, all logs in a folder | 10-24 07:52: PASS, complete folder (summary.txt) with Bloodborne and Octopath 0. |
| 505 | pipeline cache loses pending data on exit | 10-24 07:52: PASS, second launch with the same user folder preloads the cache. |
| 386 | EmitReadConstBuffer in paged buffers | 10-24 07:52: PASS, no faults, Bloodborne layer counters 0. |
| 648 | swapchain Recreate under submit_mutex | 10-21 02:30 (code: 3 Recreates locked, 0x0 surface logged and counted) + 10-22 00:16 (regress 13 titles no regressions). Done when is static. |
| 695 | tiling for system-memory residency blocks | 10-23 08:48 owner decision: stays retained in its branch (measured -0.8 %, neutral). |
| 672 | hidden direct memory pool, 512 MiB cache | 10-23 08:48 owner decision: stays retained in its branch (+1.0 %, noise). |

## Send first (jobs that close several tasks)

1. wbc3: full regress of 14 titles + Saga x6 on origin/develop 843a6100d7 or newer (j267, already queued; j280 resent): closes or advances #1218, #831, #592, #593, #595, #575, #477, #1207 (Ryza/Ayesha native Ngs2), #624 (Ryza Audiodec), and the thread counters of #1265, #1269, #1270.
2. wbc3: Bloodborne x3 perf plus thread-reuse check (j268, queued): #1269, #1270, #1265 (exit summary counters), no perf regression.
3. wbc3: Bloodborne with `PS_VRAM_LIMIT_MB=512`: #1224 (marks below budget) and a first point for #573.
4. wbc3: Bloodborne twice with the same user folder (`workdir` profile, `PS_DRIVER_PIPELINE_CACHE` unset): #1183 (cache reuse), #370 warm-cache compiles.
5. wbc3: `PS_FRAME_GEN=1`, `PS_VK_VALIDATION=1`, `PS_PRESENT_MODE=fifo` Bloodborne with frame dumps: #1182 (flicker by luma), and the NVIDIA half of #597.
6. wbc3: seeded in-game Bloodborne with `PS_FRAME_STATS=1`: #504, #750 (only if the faults appear).
7. Windows: build psport and decanter at origin/develop, `decanter ps run` Bloodborne 5 min (also with `PS_RED_ZONE_GUARD=1`), `ps report`: closes #1145 W1/W2, #968, #614, #899, #902, the Windows part of #1269/#1270, #982 and #1036.
8. Windows: GUI screenshot batch of the PS section and sidebars on develop: #753, #754, #755, #801, #432, #566, #1271.
9. Windows: Job Object stop test and the remaining #440 items (needs a multi-process game): #526, #440.
10. Night (AMD): Bloodborne perf with limited VRAM and a longer crash count: #573, #762.

## Close now (NONE with evidence)

#1035 (10-25 21:29), #992 (same comment), #1039 (10-26 16:58), #1040 (10-26 17:11), #1048 (10-28 09:42), #993 (10-25 23:14), #597 (10-29 01:54 and 10-29 09:34), #805 (10-23 07:49 and 08:17), #621 (10-24 07:52), #505 (10-24 07:52), #386 (10-24 07:52), #648 (10-22 00:16), #695 and #672 (10-23 08:48, owner decision to keep retained: close as retained or leave parked without the `testing` tag).
