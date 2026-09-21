//! Raw Win32 calls, and loading/resolving the real `Galaxy64.dll` this
//! proxy forwards to.

use std::ffi::{OsString, c_char, c_void};
use std::os::windows::ffi::OsStringExt;
use std::path::PathBuf;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicUsize, Ordering};

pub(crate) type Hmodule = *mut c_void;

unsafe extern "system" {
    fn LoadLibraryW(name: *const u16) -> Hmodule;
    fn GetProcAddress(module: Hmodule, name: *const c_char) -> *const c_void;
    fn GetModuleHandleExW(flags: u32, name: *const u16, module: *mut Hmodule) -> i32;
    fn GetModuleFileNameW(module: Hmodule, buf: *mut u16, size: u32) -> u32;
    pub(crate) fn VirtualProtect(addr: *mut c_void, size: usize, new: u32, old: *mut u32) -> i32;
}

const GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT: u32 = 0x2;
const GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS: u32 = 0x4;
pub(crate) const PAGE_READWRITE: u32 = 0x04;

/// Where this DLL itself lives. Found from the address of one of our own
/// functions rather than by name, so it holds even if someone renames the
/// proxy.
pub(crate) fn own_directory() -> Option<PathBuf> {
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

pub(crate) fn wide(path: &std::path::Path) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;
    path.as_os_str().encode_wide().chain(Some(0)).collect()
}

/// What the original DLL has to be renamed to. It has to stay in the same
/// directory: Galaxy loads further pieces relative to its own location.
const REAL_DLL: &str = "Galaxy64.orig.dll";

/// The real Galaxy64, loaded once. Deliberately not done from `DllMain`:
/// calling `LoadLibrary` under the loader lock is the classic way to
/// deadlock a process, so it happens on the first forwarded call instead.
pub(crate) fn real_module() -> Hmodule {
    static MODULE: OnceLock<usize> = OnceLock::new();
    *MODULE.get_or_init(|| {
        let Some(dir) = own_directory() else {
            return 0;
        };
        let candidate = dir.join(REAL_DLL);
        crate::log::trace(&format!("loading {}", candidate.display()));
        // SAFETY: `wide` NUL-terminates its output, and the `Vec<u16>` it
        // returns is kept alive for the duration of this call by the
        // temporary not being dropped until the statement ends.
        let handle = unsafe { LoadLibraryW(wide(&candidate).as_ptr()) as usize };
        crate::log::trace(&format!("loaded, handle {handle:#x}"));
        handle
    }) as Hmodule
}

/// Resolves one export from the real DLL, caching it. `name` must be NUL
/// terminated. A null result is normal rather than an error: the export
/// list grew across SDK versions, so a game linked against a newer Galaxy
/// makes us export symbols an older real DLL simply does not have.
pub(crate) fn real_proc(cache: &AtomicUsize, name: &str) -> *const c_void {
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
        crate::log::trace(&format!("real dll has no {}", name.trim_end_matches('\0')));
    }
    cache.store(found as usize, Ordering::Relaxed);
    found
}
