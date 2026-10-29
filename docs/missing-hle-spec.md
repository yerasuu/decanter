# Missing HLE calls: specification from the console's own modules

Source: the owner's 2024 dump of console modules (`~/.local/share/shadPS4/sys_modules/`, firmware version unknown), read with
`objdump` on the decrypted sprx files (the REA server could not connect), and the 29 October runs that load that dump through
`PS_SYS_MODULES`. Addresses are module image offsets (base 0). kb #1248; context in #1247.

## What blocks start-up under `PS_SYS_MODULES`

| Title | Proven cause |
|---|---|
| Ryza, Ayesha | `libSceCommonDialog` +0x1c90 (`mov eax,[rdi+0x28]`, rdi = 0). The function reads the SDK version at +0x28 of the CommonDialog shared-memory page. The page is NULL because `shm_open` is a stub returning 0, so `ftruncate(0, 0x1000)` fails with EINVAL and the module stores NULL; `sceCommonDialogInitialize` (+0xf20) then calls it with that NULL. |
| BO1 | `libSceHttp2` module_start returns 0x804101C8 (libSceNet "not initialized"). libSceNet's init flag (+0x3c100) is set only by `sceNetInit`, run by libSceNet's own module_start. psport starts modules in list order, so Http2 (module 5) starts before libSceNet (module 8) although Http2 lists it in NEEDED. A loader start-order problem, not a missing call. |
| Onigiri | `libSceNpAuth` module_start returns 0x80550303: `sceNpAllocateKernelMemoryWithAlignment`, `sceNpHeapInit`, then `sce::np::Object::operator new(0x18, allocator)`; a NULL from `operator new` gives that code. psport's native libSceNpCommon has none of these five exports. |
| Saga, Symphonia | `libSceHmd` module_start returns -1 when `open("/dev/hmd_cmd", O_RDWR)` fails; it also opens `/dev/hmd_snsr`, `/dev/hmd_3da`, `/dev/hmd_dist` and mmaps 0x4000 bytes of hmd_dist (PROT_READ, MAP_SHARED). |

Everything else in the unimplemented list only adds to the counts.

## A. Kernel and sysmodule

libkernel syscall stubs (set errno, return -1): `_ioctl` 54, `__sys_netcontrol` 99, `__sys_socketex` 113, `__sys_socketclose` 114,
`shm_open` 482, `shm_unlink` 483, `__sys_regmgr_call` 532, `is_in_sandbox` 585, `get_authinfo` 587, `__sys_randomized_path` 602,
`get_self_auth_info` 607, `__sys_get_proc_type_info` 612.

| Call | Proven behaviour | Priority | Implementation sketch |
|---|---|---|---|
| `shm_open(path, flags, mode)` | POSIX. CommonDialog +0x1ab0 builds `"/%08x" "CdlgShWk"` from the app id. Flags `O_RDWR`, or `O_RDWR\|O_CREAT` (0x202) when creating, plus 0x200000 if SDK >= 0x3500000 (that bit is not identified; ignore it). Mode 0666. | **blocks start-up** | `runtime_file.c`: name to host table backed by `memfd_create` (platform-layer call on Windows), adopted with `runtime_file_adopt`; honour `O_CREAT`/`O_EXCL` (EEXIST) and ENOENT. `ftruncate` and `mmap(MAP_SHARED)` then work through `runtime_file_shared_host` and `place_file`. `shm_unlink` drops the name. |
| `ftruncate(0, 0x1000)` -> -1, errno 22 | fd 0 is the stubbed `shm_open`'s 0 (stdin). FreeBSD returns EINVAL on a non-regular fd, so errno 22 is correct. | fixed by `shm_open` | none |
| `sceLncUtilGetAppStatus(LncAppStatus*)` | CommonDialog `getSelfAppId` (+0x12a0) reads only the first u32 (app id); -1 on failure; buffer at most 16 bytes. | counted; names the shm segment | zero 16 bytes, write a fixed app id, return 0 |
| `__sys_regmgr_call(op, id, int *p_status, void *p_buf, cb)` | GetInt (+0x280): op 2, buf = out, cb 4. NonSysGetInt (+0x14a0): op 0x19, id 0, buf `{u64 key; u32 index; u32 value}`, cb 0x10, returns `buf.value`. NULL out gives 0x800D0202. | counted | implement ops 2 and 0x19 from a table of known keys and defaults; always write `*p_status` and the value; unknown key logged and counted |
| `sceKernelError(e)` | `e ? 0x80020000 + e : 0` (+0x14e50) | counted | one line; dipsw failures then read 0x80020002 instead of "err = 0" |
| `get_authinfo(pid, authinfo*)` | 0x88-byte struct `{u64 paid; u64 caps[4]; u64 attrs[4]; u64 unk[8]}`. Read: `caps[0]` bits 62 and 60, `caps[1]` bit 57, `attrs` byte +0x2a. paid 0x38000000100000xx is a system process; paid>>56 == 0x31 triggers `sceKernelGetUtokenUseSoftwagnerForAcmgr`. | counted (62 calls) | zero-fill, return 0 for GUEST_PID or 0; ESRCH for other pids; EFAULT for NULL. A zeroed struct takes every retail-game branch. The console's real paid for a game was not read. |
| `__sys_get_proc_type_info(p)` | `{u64 size = 0x10; u32 type (+8); u32 flags (+0xc)}`; flags bits 0x02, 0x10, 0x20, 0x40, 0x80 select system, JIT, SpZeroConf, SpcPlayerCore behaviour. | counted | check size (EINVAL), write type 0 and flags 0, return 0 (matches `sceKernelGetProcessType`) |
| sysctl MIB 1.14.35.1000 | `KERN_PROC_APPINFO` `{CTL_KERN, KERN_PROC, 35, pid}`, oldlen 0x48. `sceKernelGetAppInfo` (+0x1e060) is this sysctl with errno through `sceKernelError`. Siblings: MIB 36 (SDK version by pid, 8 B), 45 (0x48 B, unknown), 41, 47. | counted | answer MIB 35 for GUEST_PID in `guest_sysctl`, and point `sceKernelGetAppInfo` at the same filler. Proven: +0x1d rendering-mode byte (negative gives 0x80020001), +0x20 u64, +0x30 title-workaround block with bitmap at +0x38. Inferred: app id at +0, title id at +0x10. Retail: everything 0 except app id and title id. |
| `sceKernelTitleWorkaroundIsEnabled(p, id, int *out)` | Pure (+0x1e2e0): NULL p or out gives 0x8002000E; id > 0x38 gives 0x80020016; `*out = bit id of *(u64*)(p + 8)`. Misspelled twin `sceKernelTitleWorkdaroundIsEnabled` (GST42pfUfMc) identical. | counted | implement exactly |
| `sceKernelIsInSandbox()` | `jmp is_in_sandbox` (585), no args | counted (28) | return 1 for a game (sysmodule then builds paths from the random word) |
| `sceKernelGetFsSandboxRandomWord()` | No arguments, returns `const char*`: `__sys_randomized_path(0, static buf, &len = 0xff)`; -1 gives NULL, len 0 gives "". Used by sysmodule paths and DiscMap `"/%s/common_temp/sceDiscMapBitmapInfo"`. | counted | return a static guest string (a fixed word); NULL now makes DiscMap say "get path failed" |
| `sceKernelLoadStartModuleForSysmodule(path, args, argp, flags, opt, int *res)` | Same as `sceKernelLoadStartModule` with `flags \|= 0x10000`; the public call rejects flags != 0 with EINVAL, this one does not. Non-NULL opt gives EINVAL. Errors through `sceKernelError` (ENOENT 0x80020002; 0x80020023 table full). | counted (28) | reuse `load_start_module` in `runtime_module.c` without the flags check; `find_path` already matches by base name |
| `sceKernelGetExecutableModuleHandle()` | returns the main module handle | counted | return 0 (psport's eboot index) |
| `sceKernelGetModuleInfo(handle, SceKernelModuleInfo*)` | Caller presets size 0x160: `{u64 size; char name[256]; {u64 addr; u32 size; i32 prot} seg[4]; u32 nseg; u8 fingerprint[20]}`. Errno map EPERM 0x80020001, ESRCH 0x80020003, EINVAL 0x80020016. **A non-zero return makes sysmodule init return -1.** | counted today, would block if it errors | fill from `RuntimeModule` as `module_info_from_address` does |
| `sceKernelGetVrCaptureSize(out_a[0x28], out_b[0x28])` | copies two libkernel globals; libSceCamera init skips VR capture on size 0 or negative return | counted | zero both blocks, return 0 |

## B. Devices

| Device | Proven need | Priority | Sketch |
|---|---|---|---|
| `/dev/dipsw` | sysmodule +0x4d10: `_open(O_RDONLY)`, `_ioctl(fd, 0x40048806 = _IOR(0x88, 6, int))`, close. Indices 7 (0x40048807), 8 (0x40088808), 9 (0x40088809) read only when index 6 == 1 (devkit). On failure returns `sceKernelError(errno)` and prints `dipsw [0x%02x]: %s failed, err = 0x%08x`; the caller checks only `== 1`. | counted (failure already behaves like retail) | virtual device whose four ioctls write 0; otherwise leave ENOENT and implement `sceKernelError` |
| `/dev/hmd_cmd`, `hmd_snsr`, `hmd_3da`, `hmd_dist` | opened O_RDWR; hmd_dist mmapped 0x4000 PROT_READ MAP_SHARED; ioctl 0x8001A02A only when appinfo+0xc & 0x30 == 0x10 (VR title) | **blocks start-up** (dumped libSceHmd only) | preferred: do not link libSceHmd.sprx; psport already serves it natively (`runtime_vr.c`, no headset). Alternative: four virtual fds with a zero 16 KiB page for hmd_dist and ioctls answering "disconnected" (codes unread). |
| `/dev/camera` | libSceCamera +0x62a opens O_RDWR; failure goes to the error path but module_start still returns 0 | counted | psport already answers "no camera" natively (`runtime_devices.c`); ENOENT acceptable |

## C. Video and system

| Call | Proven | Sketch |
|---|---|---|
| `sceVideoOutAddDriver(bus_type = 4, ops*, ctx*, must_be_NULL)` | `ops` is 0x108 bytes of callbacks, +0, 8, 0x10, 0x18, 0x20, 0x28, 0x38, 0x40 mandatory; ctx 32 bytes, may be NULL. Registers one driver per bus type (+0xa820), returns a 64-bit slot value. | validate the mandatory callbacks, record the table, never call it, return 0 (what works today); check the real return on wbc3 |
| `sceVideoOutDeleteDriver` | errors 0x8029000A, 0x80290001, 0x80290009 | forget the slot, return 0 |
| `sceVshAvcapInit()` | no args, reference counted (libSceAvcap +0x940); connects IPMI to `SceAvCaptureIpc` with a 64 KiB mspace | psport already serves libSceAvcap: add Init/Term as a counter returning 0; other calls return "not available" (code unread) |

## D. Network control (offline)

`sceNetInit` (+0x7b0) runs in libSceNet's own module_start: `netcontrol(-1, 0x14, &int, 4)`; `socketex("SceNetInit", 2, 6, 0)`;
`_ioctl(fd, 0x802450C9 = _IOW('P', 0xC9, 36), {16 random bytes...})`; `socketclose(fd)`. If the socket or the ioctl fails, `sceNetInit`
fails; libSceNet maps raw errno to 0x80410100|errno and retries EINTR.

| Call | Sketch |
|---|---|
| `__sys_socketex(name, domain, type, proto)` | use `do_socket` in `runtime_socket.c` (type 6 already maps to datagram); POSIX return |
| `__sys_socketclose(fd)` | use `do_close` |
| `_ioctl(fd, cmd, arg)` | sockets: accept 0x802450C9 with 0; dipsw as in B; anything else ENOTTY, counted |
| `__sys_netcontrol(-1, cmd, buf, cb)` | cmd 0x14: write int 0; cmd 1 (cb 0x10, memory statistics): zero it; others return 0 with a zeroed buffer, logged and counted |

## E. Np (Onigiri; add to the native libSceNpCommon in `runtime_np.c`)

| Call | Proven behaviour |
|---|---|
| `sceNpAllocateKernelMemoryWithAlignment(size, name, void **out, NpKernelMem *m)` | NULL m gives 0x80558005. `m->size = roundup(size, 0x4000)`, `m->[+8] = 0`, `sceKernelMapNamedFlexibleMemory(&m->addr (+0x10), m->size, RW, 0, name)`; on success `*out = m->addr`; returns the map result. The NoAlignment variant is the same without rounding. |
| `sceNpFreeKernelMemory(m)` | NULL gives 0x80558005; unmaps `addr`/`size` if set, sets `addr = 0`, returns 0 |
| `sceNpHeapInit(heap*, base, size, name)` | `heap->mspace = sceLibcMspaceCreate(name, base, size, 0)`; NULL gives 0x8055800A |
| `sceNpHeapDestroy(heap*)` | destroys the mspace if set, NULLs it; returns void |
| `sceNpHeapGetAllocator(heap*, SceNpAllocator *a)` | `a = {malloc(sz, user), realloc(p, sz, user), free(p, user), user = heap}` (0x20 bytes) |
| `_ZN3sce2np6ObjectnwEmR14SceNpAllocator` (the `[]` variant identical) | `p = a.malloc(sz + 0x10, a.user)`; `*(SceNpAllocator**)p = &a`; returns `p + 0x10` or NULL |
| `delete` (both forms) | `hdr = *(p - 0x10)`; `hdr->free(p - 0x10, hdr->user)` |

psport has no mspace implementation; the game's libc.prx exports `sceLibcMspace*`. A native NpHeap needs a small host-side
allocator over the guest buffer (first-fit or TLSF), since the heap must live in that guest memory.

## F. `sceKernelOpen` ENOENT

Both are the right answer. `/app0/sce_discmap_patch.plt`: DiscMap (+0x538) opens it after `sce_discmap.plt` and does not stop on
failure; on a console it exists only when a patch is installed (Onigiri and CUSA03173 ship one; Ryza, Ayesha, BO1 do not).
`sampleArg.txt`: Ayesha's PhyreEngine/PSSG reads it optionally (string at eboot 0x9fecc3, next to `PApplication::onInit`); a
developer file absent from retail packages.

## Outside the list: module start order (blocks BO1)

`main.c` starts linked modules in table order ("as the PS4 dynamic linker does"). That holds for the game's own modules, but the
appended system sprx files are not ordered by their NEEDED entries: libSceHttp2 starts before libSceNet, CommonDialog before
libSceRegMgr. Proposal: start dependencies first (post-order over NEEDED/IMPORT_MODULE).

## Proven and inferred

Proven: every struct size and read offset, the error codes, the syscall numbers, the four blocking chains, errno 22 for fd 0.
Inferred: appinfo fields at +0 and +0x10, +0x20 as preload flags; the meaning of each authinfo bit and proc-type flag; that zero is
the console's value for a retail game (type, flags, paid); the AddDriver return semantics; that the Hmd ioctls answer
"disconnected".

Still unread: the codes `__sys_regmgr_call` returns for unknown keys; the 0x200000 shm flag; the Hmd ioctl set beyond init; the
Avcap error codes; the purpose of KERN_PROC MIB 45; the full AddDriver return path.

## What to check on wbc3 or a night

1. With `PS_SYS_MODULES`, after `shm_open`: Ryza and Ayesha get past `sceCommonDialogInitialize`.
2. With dependency-ordered start: BO1's Http2 initializer returns 0.
3. After the NpCommon exports: Onigiri's NpAuth init returns 0.
4. With the dumped libSceHmd not linked: Saga and Symphonia start.
5. Without `PS_SYS_MODULES`, the 13-title run of n1033 stays green (regression check).
