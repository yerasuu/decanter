//! A proxy `Galaxy64.dll` that records achievement unlocks as the game
//! triggers them.
//!
//! GOG games with Galaxy integration ship `Galaxy64.dll` and call
//! `IStats::SetAchievement` on it when the player earns something. On
//! Windows that DLL talks over local IPC to the running GOG Galaxy client,
//! which is what actually reports the unlock. Under Wine there is no
//! Galaxy client listening, so the call goes nowhere and the unlock is
//! lost — the game believes it reported it.
//!
//! This sits in the middle: rename the real DLL to `Galaxy64.orig.dll`,
//! drop this in as `Galaxy64.dll`, and every export is forwarded
//! unchanged except that `Stats()` patches one vtable slot on the way
//! through. Nothing is sent anywhere yet — unlocks are appended to a
//! JSONL file for decanter to pick up later.
//!
//! Two rules hold everywhere in here: never panic (a panic unwinding into
//! the game's C++ frames is undefined), and never let a failure of ours
//! stop the game — every call is forwarded whether or not the logging
//! worked, and anything we cannot make sense of leaves the game unhooked
//! rather than guessed at.
//!
//! The vtable layout, and how it was derived from the binary, is in
//! `docs/galaxy64-abi.md`.

// The crate has to be called `Galaxy64` for the built DLL to be the name
// the game imports; that is not a snake-case identifier and cannot be.
#![allow(non_snake_case)]

use std::ffi::{OsString, c_char, c_void};
use std::os::windows::ffi::OsStringExt;
use std::path::PathBuf;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicUsize, Ordering};

// ------------------------------------------------------------------ Win32

type Hmodule = *mut c_void;

unsafe extern "system" {
    fn LoadLibraryW(name: *const u16) -> Hmodule;
    fn GetProcAddress(module: Hmodule, name: *const c_char) -> *const c_void;
    fn GetModuleHandleExW(flags: u32, name: *const u16, module: *mut Hmodule) -> i32;
    fn GetModuleFileNameW(module: Hmodule, buf: *mut u16, size: u32) -> u32;
    fn VirtualProtect(addr: *mut c_void, size: usize, new: u32, old: *mut u32) -> i32;
}

const GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT: u32 = 0x2;
const GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS: u32 = 0x4;
const PAGE_READWRITE: u32 = 0x04;

/// Where this DLL itself lives. Found from the address of one of our own
/// functions rather than by name, so it holds even if someone renames the
/// proxy.
fn own_directory() -> Option<PathBuf> {
    let mut module: Hmodule = std::ptr::null_mut();
    // SAFETY: `own_directory` is a valid code address in this module and
    // `module` is a live `&mut` for the call to write into; the flags ask
    // the loader for a lookup by address, not a load, so no refcount is at
    // stake either way.
    let ok = unsafe {
        GetModuleHandleExW(
            GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS | GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
            own_directory as *const u16,
            &mut module,
        )
    };
    if ok == 0 {
        return None;
    }

    let mut buf = [0u16; 32768];
    // SAFETY: `module` was just handed back by the loader above, and `buf`
    // is a real 32768-`u16` stack buffer whose length is passed alongside
    // the pointer, so the call cannot write past it.
    let len = unsafe { GetModuleFileNameW(module, buf.as_mut_ptr(), buf.len() as u32) };
    if len == 0 || len as usize >= buf.len() {
        return None;
    }
    PathBuf::from(OsString::from_wide(&buf[..len as usize]))
        .parent()
        .map(PathBuf::from)
}

fn wide(path: &std::path::Path) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;
    path.as_os_str().encode_wide().chain(Some(0)).collect()
}

/// What the original DLL has to be renamed to. It has to stay in the same
/// directory: Galaxy loads further pieces relative to its own location.
const REAL_DLL: &str = "Galaxy64.orig.dll";

/// The real Galaxy64, loaded once. Deliberately not done from `DllMain`:
/// calling `LoadLibrary` under the loader lock is the classic way to
/// deadlock a process, so it happens on the first forwarded call instead.
fn real_module() -> Hmodule {
    static MODULE: OnceLock<usize> = OnceLock::new();
    *MODULE.get_or_init(|| {
        let Some(dir) = own_directory() else {
            return 0;
        };
        let candidate = dir.join(REAL_DLL);
        trace(&format!("loading {}", candidate.display()));
        // SAFETY: `wide` NUL-terminates its output, and the `Vec<u16>` it
        // returns is kept alive for the duration of this call by the
        // temporary not being dropped until the statement ends.
        let handle = unsafe { LoadLibraryW(wide(&candidate).as_ptr()) as usize };
        trace(&format!("loaded, handle {handle:#x}"));
        handle
    }) as Hmodule
}

/// Resolves one export from the real DLL, caching it. `name` must be NUL
/// terminated. A null result is normal rather than an error: the export
/// list grew across SDK versions, so a game linked against a newer Galaxy
/// makes us export symbols an older real DLL simply does not have.
fn real_proc(cache: &AtomicUsize, name: &str) -> *const c_void {
    let cached = cache.load(Ordering::Relaxed);
    if cached != 0 {
        return cached as *const c_void;
    }
    let module = real_module();
    if module.is_null() {
        return std::ptr::null();
    }
    // SAFETY: `module` was just null-checked, and callers pass NUL-terminated
    // literals (see the doc comment on `name`).
    let found = unsafe { GetProcAddress(module, name.as_ptr() as *const c_char) };
    if found.is_null() {
        trace(&format!("real dll has no {}", name.trim_end_matches('\0')));
    }
    cache.store(found as usize, Ordering::Relaxed);
    found
}

// ---------------------------------------------------------------- logging

/// Appends one unlock. Best effort in every direction: a missing path, an
/// unwritable file or a non-UTF-8 achievement id all end with the game
/// continuing exactly as it would have.
fn record_unlock(name: *const c_char) {
    if name.is_null() {
        return;
    }
    // SAFETY: the pointer comes from the game, which owns a NUL-terminated
    // string here per the Galaxy API contract.
    let Ok(name) = (unsafe { std::ffi::CStr::from_ptr(name) }).to_str() else {
        return;
    };
    let product = std::env::var("DECANTER_PRODUCT_ID").unwrap_or_default();
    append(&format!(
        r#"{{"unlocked_at":{},"achievement":"{}","product_id":"{}"}}"#,
        now(),
        escape(name),
        escape(&product),
    ));
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn append(line: &str) {
    use std::io::Write;
    let Some(path) = log_path() else {
        return;
    };
    let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(path) else {
        return;
    };
    let _ = writeln!(file, "{line}");
    let _ = file.flush();
}

/// A breadcrumb, so a run that never unlocks anything still says how far it
/// got. Written to the same file and told apart by carrying `note` rather
/// than `achievement`. Flushed per line: the point is to survive a hang.
fn trace(note: &str) {
    append(&format!(r#"{{"at":{},"note":"{}"}}"#, now(), escape(note)));
}

/// Traces the first time a given export is entered, so the per-frame pumps
/// do not bury the sequence in noise.
fn trace_once(seen: &AtomicUsize, what: &str) {
    if seen.swap(1, Ordering::Relaxed) == 0 {
        trace(what);
    }
}

/// Achievement ids are conventionally bare identifiers, but nothing
/// enforces that -- escape rather than trust it and emit broken JSON.
fn escape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for c in value.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

fn log_path() -> Option<PathBuf> {
    if let Ok(explicit) = std::env::var("DECANTER_ACHIEVEMENT_LOG")
        && !explicit.is_empty()
    {
        return Some(PathBuf::from(explicit));
    }
    // Beside the DLL, so a run with nothing configured still leaves the
    // evidence somewhere findable rather than dropping it.
    own_directory().map(|dir| dir.join("decanter-achievements.jsonl"))
}

// ------------------------------------------------- finding the right slot
//
// `SetAchievement`'s position in the `IStats` vtable is NOT stable across
// Galaxy SDK versions, and getting it wrong is not a soft failure: it
// overwrites an unrelated method that has a different signature.
//
// Two real DLLs, shipped by two GOG games on this machine, disagree:
//
//   35-slot IStats -> SetAchievement is slot 8
//   38-slot IStats -> SetAchievement is slot 11, because a later SDK
//                     inserted GetAchievementsNumber, GetAchievementName
//                     and GetAchievementNameCopy ahead of it
//
// So the slot is discovered rather than assumed, using the same property
// the offline map in `docs/galaxy64-abi.md` relies on: every Galaxy method
// passes its own name to an internal logging call, so the body of
// `SetAchievement` references the literal string `"SetAchievement"`. That
// is a fact about the binary in front of us rather than about a version
// number, and if it cannot be established the hook is not installed.

/// The name the method we want announces itself by.
const SET_ACHIEVEMENT: &[u8] = b"SetAchievement";

/// No real `IStats` is anywhere near this long; the bound just stops a
/// malformed vtable from walking off into unrelated memory.
const MAX_SLOTS: usize = 96;

/// How far into a method body to look for its name. The logging call comes
/// early, right after the prologue.
const SCAN_WINDOW: usize = 0x600;

/// Base address and length of a loaded module, read from its own PE
/// headers. Every address the scan below dereferences is checked against
/// this range first.
fn module_bounds(module: Hmodule) -> Option<(usize, usize)> {
    let base = module as usize;
    if base == 0 {
        return None;
    }
    // SAFETY: `base` is a loaded module's handle, i.e. its mapped image
    // base, so its first page is readable; `IMAGE_DOS_HEADER::e_lfanew`
    // sits at 0x3c in every PE.
    let pe_offset = unsafe { std::ptr::read_unaligned((base + 0x3c) as *const u32) } as usize;
    if pe_offset == 0 || pe_offset > 0x1000 {
        return None;
    }
    let pe = base + pe_offset;
    // SAFETY: `pe_offset` was just bounded to the first page, still inside
    // the mapped image checked above.
    if unsafe { std::ptr::read_unaligned(pe as *const u32) } != 0x0000_4550 {
        return None;
    }
    // SAFETY: same page as the signature check just above; `SizeOfImage`
    // lives at a fixed offset from the PE signature in every PE32+ image.
    let size = unsafe { std::ptr::read_unaligned((pe + 24 + 56) as *const u32) } as usize;
    (size != 0).then(|| (base, base + size))
}

/// Follows `jmp rel32` chains: every export and virtual in these DLLs goes
/// through one of the linker's thunk tables first.
fn resolve_thunk(mut address: usize, lo: usize, hi: usize) -> usize {
    for _ in 0..8 {
        if address < lo || address + 5 > hi {
            return address;
        }
        // SAFETY: the bound above guarantees `[address, address+5)` sits
        // inside `[lo, hi)`, the caller-checked module range.
        if unsafe { std::ptr::read_unaligned(address as *const u8) } != 0xE9 {
            return address;
        }
        // SAFETY: same 5-byte range just checked above.
        let relative = unsafe { std::ptr::read_unaligned((address + 1) as *const i32) };
        address = (address + 5).wrapping_add(relative as usize);
    }
    address
}

/// Reads a NUL-terminated ASCII identifier, if that is what sits at
/// `address`. Anything else -- punctuation, a format string, binary --
/// gives `None`, which is what keeps the match below from being a
/// coincidence.
fn identifier_at(address: usize, hi: usize) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(32);
    for offset in 0..64usize {
        if address + offset >= hi {
            return None;
        }
        // SAFETY: bounded against `hi` just above; `address` itself is
        // caller-checked against the module range before this is reached.
        let byte = unsafe { std::ptr::read_unaligned((address + offset) as *const u8) };
        match byte {
            0 => break,
            b'A'..=b'Z' if offset == 0 => out.push(byte),
            b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'_' if offset > 0 => out.push(byte),
            _ => return None,
        }
    }
    (out.len() >= 3).then_some(out)
}

/// The first identifier-shaped string a function body references through a
/// `lea reg, [rip+disp32]`. Mirrors the offline tool exactly, so what runs
/// here and what produced the committed map cannot drift apart.
fn first_referenced_identifier(function: usize, lo: usize, hi: usize) -> Option<Vec<u8>> {
    if function < lo || function >= hi {
        return None;
    }
    let end = (function + SCAN_WINDOW).min(hi);
    let mut at = function;
    while at + 7 <= end {
        // SAFETY: the loop condition guarantees `[at, at+7)` fits before
        // `end`, which is itself clamped to `hi`.
        let opcode = unsafe { std::ptr::read_unaligned(at as *const [u8; 3]) };
        // REX.W / REX.WR, `lea`, and a modrm selecting rip-relative
        // addressing (mod = 00, rm = 101) for any destination register.
        let rip_relative = matches!(opcode[0], 0x48 | 0x4C)
            && opcode[1] == 0x8D
            && matches!(opcode[2], 0x05 | 0x0D | 0x15 | 0x1D | 0x25 | 0x2D | 0x35 | 0x3D);
        if rip_relative {
            // SAFETY: same 7-byte window as the opcode read above.
            let displacement = unsafe { std::ptr::read_unaligned((at + 3) as *const i32) };
            let target = (at + 7).wrapping_add(displacement as usize);
            if target >= lo
                && target < hi
                && let Some(identifier) = identifier_at(target, hi)
            {
                return Some(identifier);
            }
        }
        at += 1;
    }
    None
}

/// Which vtable slot holds `SetAchievement`, or `None` if this binary
/// cannot be read confidently -- in which case nothing gets patched.
fn discover_set_achievement_slot(vtable: *const usize, lo: usize, hi: usize) -> Option<usize> {
    for slot in 0..MAX_SLOTS {
        // SAFETY: `vtable` is the object's first field (a live vtable
        // pointer per the caller in `install_hook`), and `MAX_SLOTS` bounds
        // the walk far below any real `IStats` vtable — the loop bails via
        // the range check below the first time it runs past the real one.
        let entry = unsafe { std::ptr::read_unaligned(vtable.add(slot)) };
        // The end of the vtable: the next qword is no longer code.
        if entry < lo || entry >= hi {
            return None;
        }
        let function = resolve_thunk(entry, lo, hi);
        if first_referenced_identifier(function, lo, hi).as_deref() == Some(SET_ACHIEVEMENT) {
            return Some(slot);
        }
    }
    None
}

// ------------------------------------------------------------- the hook

/// The real `SetAchievement`, saved before the slot is overwritten.
static ORIGINAL_SET_ACHIEVEMENT: AtomicUsize = AtomicUsize::new(0);

/// Stands in for `IStats::SetAchievement(const char* name)`. MSVC compiles
/// a member function to the ordinary x64 convention with `this` in `rcx`,
/// which is exactly what `extern "C"` gives us here.
unsafe extern "C" fn hooked_set_achievement(this: *mut c_void, name: *const c_char) {
    record_unlock(name);

    let original = ORIGINAL_SET_ACHIEVEMENT.load(Ordering::Acquire);
    if original == 0 {
        return;
    }
    // SAFETY: `original` was saved by `install_hook` from the live vtable
    // slot before it was overwritten, so it is the real `SetAchievement`
    // and matches this signature per the MSVC member-function ABI note
    // above.
    let original: unsafe extern "C" fn(*mut c_void, *const c_char) =
        unsafe { std::mem::transmute(original) };
    // Forwarded even though nothing is listening on the other side under
    // Wine: whatever the real implementation does with its own state
    // (caching the unlock for a later `StoreStatsAndAchievements`) is not
    // ours to skip.
    // SAFETY: `this` and `name` are exactly the arguments the game itself
    // passed into `hooked_set_achievement`, forwarded unchanged.
    unsafe { original(this, name) }
}

/// Swaps our function into the live vtable. Runs once, and gives up
/// quietly at every step it cannot verify -- an unreadable module, a slot
/// it cannot identify, an unwritable vtable. The game then runs exactly as
/// it would have without the proxy, minus the recording.
fn install_hook(stats: *mut c_void) {
    if stats.is_null() {
        return;
    }
    // `get_or_init`, not a check-then-act on `get()`: two threads racing
    // into `Stats()` before the first hook install finishes must not both
    // patch the vtable -- the second would read back its own already-
    // installed hook as "the original" and recurse into itself forever.
    static INSTALLED: OnceLock<()> = OnceLock::new();
    INSTALLED.get_or_init(|| install_vtable_patch(stats));
}

fn install_vtable_patch(stats: *mut c_void) {
    let Some((lo, hi)) = module_bounds(real_module()) else {
        trace("could not read the real dll's headers; running unhooked");
        return;
    };

    // The object's first field is its vtable pointer.
    // SAFETY: `stats` was just null-checked and comes from the game's own
    // `Stats()` accessor, so it points at a live `IStats` instance.
    let vtable = unsafe { std::ptr::read_unaligned(stats as *const *mut usize) };
    if vtable.is_null() {
        return;
    }

    let Some(slot_index) = discover_set_achievement_slot(vtable, lo, hi) else {
        trace("could not find SetAchievement in the vtable; running unhooked");
        return;
    };
    trace(&format!("SetAchievement is vtable slot {slot_index}"));

    // SAFETY: `slot_index` was returned by `discover_set_achievement_slot`,
    // which only returns slots it read within `[0, MAX_SLOTS)` of this same
    // vtable.
    let slot = unsafe { vtable.add(slot_index) };
    // vtables live in the image's read-only data.
    let mut previous = 0u32;
    // SAFETY: `slot` is a valid pointer into the vtable per the above, and
    // `previous` is a live `&mut` for the call to write the prior
    // protection into.
    let unprotected = unsafe {
        VirtualProtect(
            slot as *mut c_void,
            std::mem::size_of::<usize>(),
            PAGE_READWRITE,
            &mut previous,
        )
    };
    if unprotected == 0 {
        trace("VirtualProtect failed; running unhooked");
        return;
    }

    // SAFETY: `VirtualProtect` above just made `slot` writable.
    ORIGINAL_SET_ACHIEVEMENT.store(unsafe { *slot }, Ordering::Release);
    // SAFETY: same writable slot as the read just above.
    unsafe { *slot = hooked_set_achievement as *const () as usize };

    let mut ignored = 0u32;
    // SAFETY: restores the protection `VirtualProtect` reported as `previous`
    // above, on the same slot and size.
    unsafe {
        VirtualProtect(
            slot as *mut c_void,
            std::mem::size_of::<usize>(),
            previous,
            &mut ignored,
        )
    };

    trace("hook installed");
}

// --------------------------------------------------------------- exports
//
// Every export any known Galaxy64 version has, forwarded so the game links
// and runs exactly as before. This is deliberately the union across
// versions rather than one version's list: a game linked against a newer
// SDK imports symbols an older DLL never had, and a proxy missing even one
// of them fails to load, taking the game down before a single line of ours
// runs. That is not hypothetical -- it is what a 31-export game did to a
// 28-export proxy. Forwarding a symbol the real DLL lacks is harmless: the
// lookup fails once, is cached, and the stub returns null or does nothing.

/// An accessor returning an interface pointer -- the shape of most Galaxy
/// exports.
macro_rules! accessor {
    ($rust_name:ident, $symbol:literal) => {
        #[unsafe(export_name = $symbol)]
        pub extern "C" fn $rust_name() -> *mut c_void {
            static CACHE: AtomicUsize = AtomicUsize::new(0);
            static SEEN: AtomicUsize = AtomicUsize::new(0);
            trace_once(&SEEN, concat!("enter ", $symbol));
            let target = real_proc(&CACHE, concat!($symbol, "\0"));
            if target.is_null() {
                return std::ptr::null_mut();
            }
            // SAFETY: `target` is a non-null export resolved by symbol name
            // from the real Galaxy64, whose accessor exports all share this
            // `() -> *mut c_void` signature.
            let target: extern "C" fn() -> *mut c_void = unsafe { std::mem::transmute(target) };
            target()
        }
    };
}

/// `void f()` -- the process/shutdown pumps.
macro_rules! procedure {
    ($rust_name:ident, $symbol:literal) => {
        #[unsafe(export_name = $symbol)]
        pub extern "C" fn $rust_name() {
            static CACHE: AtomicUsize = AtomicUsize::new(0);
            static SEEN: AtomicUsize = AtomicUsize::new(0);
            trace_once(&SEEN, concat!("enter ", $symbol));
            let target = real_proc(&CACHE, concat!($symbol, "\0"));
            if target.is_null() {
                return;
            }
            // SAFETY: `target` is a non-null export resolved by symbol name
            // from the real Galaxy64, whose `void f()` exports all share
            // this signature.
            let target: extern "C" fn() = unsafe { std::mem::transmute(target) };
            target()
        }
    };
}

/// `void f(const T&)` -- a reference is a pointer at this level, which is
/// all the forwarding needs to know. Covers both `InitOptions` and the
/// newer `ShutdownOptions`.
macro_rules! with_options {
    ($rust_name:ident, $symbol:literal) => {
        #[unsafe(export_name = $symbol)]
        pub extern "C" fn $rust_name(options: *const c_void) {
            static CACHE: AtomicUsize = AtomicUsize::new(0);
            static SEEN: AtomicUsize = AtomicUsize::new(0);
            trace_once(&SEEN, concat!("enter ", $symbol));
            let target = real_proc(&CACHE, concat!($symbol, "\0"));
            if target.is_null() {
                return;
            }
            // SAFETY: `target` is a non-null export resolved by symbol name
            // from the real Galaxy64, whose `void f(const T&)` exports all
            // share this signature (a reference is a pointer at this ABI
            // level).
            let target: extern "C" fn(*const c_void) = unsafe { std::mem::transmute(target) };
            target(options)
        }
    };
}

/// The one export that isn't a pure forward: the game asks for `IStats`
/// here, which is the only moment a real `IStats` pointer exists to patch.
#[unsafe(export_name = "?Stats@api@galaxy@@YAPEAVIStats@12@XZ")]
pub extern "C" fn stats() -> *mut c_void {
    static CACHE: AtomicUsize = AtomicUsize::new(0);
    static SEEN: AtomicUsize = AtomicUsize::new(0);
    trace_once(&SEEN, "enter ?Stats@api@galaxy@@YAPEAVIStats@12@XZ");
    let target = real_proc(&CACHE, "?Stats@api@galaxy@@YAPEAVIStats@12@XZ\0");
    if target.is_null() {
        return std::ptr::null_mut();
    }
    // SAFETY: `target` is the non-null `?Stats@...` export resolved by name
    // from the real Galaxy64, matching this `() -> *mut c_void` signature.
    let target: extern "C" fn() -> *mut c_void = unsafe { std::mem::transmute(target) };
    let stats = target();
    install_hook(stats);
    stats
}

accessor!(apps, "?Apps@api@galaxy@@YAPEAVIApps@12@XZ");
accessor!(chat, "?Chat@api@galaxy@@YAPEAVIChat@12@XZ");
accessor!(
    cloud_storage,
    "?CloudStorage@api@galaxy@@YAPEAVICloudStorage@12@XZ"
);
accessor!(
    custom_networking,
    "?CustomNetworking@api@galaxy@@YAPEAVICustomNetworking@12@XZ"
);
accessor!(friends, "?Friends@api@galaxy@@YAPEAVIFriends@12@XZ");
accessor!(
    game_server_listener_registrar,
    "?GameServerListenerRegistrar@api@galaxy@@YAPEAVIListenerRegistrar@12@XZ"
);
accessor!(
    game_server_logger,
    "?GameServerLogger@api@galaxy@@YAPEAVILogger@12@XZ"
);
accessor!(
    game_server_matchmaking,
    "?GameServerMatchmaking@api@galaxy@@YAPEAVIMatchmaking@12@XZ"
);
accessor!(
    game_server_networking,
    "?GameServerNetworking@api@galaxy@@YAPEAVINetworking@12@XZ"
);
accessor!(
    game_server_telemetry,
    "?GameServerTelemetry@api@galaxy@@YAPEAVITelemetry@12@XZ"
);
accessor!(
    game_server_user,
    "?GameServerUser@api@galaxy@@YAPEAVIUser@12@XZ"
);
accessor!(
    game_server_utils,
    "?GameServerUtils@api@galaxy@@YAPEAVIUtils@12@XZ"
);
accessor!(get_error, "?GetError@api@galaxy@@YAPEBVIError@12@XZ");
accessor!(
    listener_registrar,
    "?ListenerRegistrar@api@galaxy@@YAPEAVIListenerRegistrar@12@XZ"
);
accessor!(logger, "?Logger@api@galaxy@@YAPEAVILogger@12@XZ");
accessor!(
    matchmaking,
    "?Matchmaking@api@galaxy@@YAPEAVIMatchmaking@12@XZ"
);
accessor!(networking, "?Networking@api@galaxy@@YAPEAVINetworking@12@XZ");
accessor!(storage, "?Storage@api@galaxy@@YAPEAVIStorage@12@XZ");
accessor!(telemetry, "?Telemetry@api@galaxy@@YAPEAVITelemetry@12@XZ");
accessor!(user, "?User@api@galaxy@@YAPEAVIUser@12@XZ");
accessor!(utils, "?Utils@api@galaxy@@YAPEAVIUtils@12@XZ");

procedure!(process_data, "?ProcessData@api@galaxy@@YAXXZ");
procedure!(
    process_game_server_data,
    "?ProcessGameServerData@api@galaxy@@YAXXZ"
);
procedure!(shutdown, "?Shutdown@api@galaxy@@YAXXZ");
procedure!(shutdown_game_server, "?ShutdownGameServer@api@galaxy@@YAXXZ");
// The real one is a bare `ret` -- kept only so the export table matches.
procedure!(load, "load");

with_options!(init, "?Init@api@galaxy@@YAXAEBUInitOptions@12@@Z");
with_options!(
    init_game_server,
    "?InitGameServer@api@galaxy@@YAXAEBUInitOptions@12@@Z"
);
with_options!(
    shutdown_ex,
    "?ShutdownEx@api@galaxy@@YAXAEBUShutdownOptions@12@@Z"
);
with_options!(
    shutdown_game_server_ex,
    "?ShutdownGameServerEx@api@galaxy@@YAXAEBUShutdownOptions@12@@Z"
);

