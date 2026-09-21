//! Patching `SetAchievement` into the live `IStats` vtable.

use std::ffi::{c_char, c_void};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicUsize, Ordering};

use crate::abi_scan::{discover_set_achievement_slot, module_bounds};
use crate::ffi::{PAGE_READWRITE, VirtualProtect, real_module};
use crate::log::{record_unlock, report_diagnostic, trace};

/// The real `SetAchievement`, saved before the slot is overwritten.
static ORIGINAL_SET_ACHIEVEMENT: AtomicUsize = AtomicUsize::new(0);

/// Stands in for `IStats::SetAchievement(const char* name)`. MSVC compiles
/// a member function to the ordinary x64 convention with `this` in `rcx`,
/// which is exactly what `extern "C"` gives us here.
pub(crate) unsafe extern "C" fn hooked_set_achievement(this: *mut c_void, name: *const c_char) {
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
pub(crate) fn install_hook(stats: *mut c_void) {
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
    let module = real_module();
    let Some((lo, hi)) = module_bounds(module) else {
        trace("could not read the real dll's headers; running unhooked");
        report_diagnostic(
            "read_headers_failed",
            &format!("real dll module handle: {:#x}", module as usize),
        );
        return;
    };

    // The object's first field is its vtable pointer.
    // SAFETY: `stats` was just null-checked and comes from the game's own
    // `Stats()` accessor, so it points at a live `IStats` instance.
    let vtable = unsafe { std::ptr::read_unaligned(stats as *const *mut usize) };
    if vtable.is_null() {
        trace("IStats has a null vtable pointer; running unhooked");
        report_diagnostic(
            "null_vtable",
            &format!("real dll module bounds: {lo:#x}..{hi:#x}"),
        );
        return;
    }

    let Some(slot_index) = discover_set_achievement_slot(vtable, lo, hi) else {
        trace("could not find SetAchievement in the vtable; running unhooked");
        // Everything a future pass over `docs/galaxy64-abi.md`-style
        // analysis needs to make sense of *this* real dll, without the
        // binary itself: where it's mapped, how big it is, and where its
        // vtable sits within that range -- enough to re-run the same
        // disassembly-based search offline against a copy of the file.
        report_diagnostic(
            "slot_not_found",
            &format!("module bounds: {lo:#x}..{hi:#x}, vtable: {:#x}", vtable as usize),
        );
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
        report_diagnostic("protect_failed", &format!("slot {slot_index} at {:#x}", slot as usize));
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
