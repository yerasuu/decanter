//! Finding `SetAchievement`'s slot in the `IStats` vtable.
//!
//! Its position is NOT stable across Galaxy SDK versions, and getting it
//! wrong is not a soft failure: it overwrites an unrelated method that has
//! a different signature.
//!
//! Two real DLLs, shipped by two GOG games on this machine, disagree:
//!
//!   35-slot IStats -> SetAchievement is slot 8
//!   38-slot IStats -> SetAchievement is slot 11, because a later SDK
//!                     inserted GetAchievementsNumber, GetAchievementName
//!                     and GetAchievementNameCopy ahead of it
//!
//! So the slot is discovered rather than assumed, using the same property
//! the offline map in `docs/galaxy64-abi.md` relies on: every Galaxy method
//! passes its own name to an internal logging call, so the body of
//! `SetAchievement` references the literal string `"SetAchievement"`. That
//! is a fact about the binary in front of us rather than about a version
//! number, and if it cannot be established the hook is not installed.

use crate::ffi::Hmodule;

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
pub(crate) fn module_bounds(module: Hmodule) -> Option<(usize, usize)> {
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
pub(crate) fn discover_set_achievement_slot(vtable: *const usize, lo: usize, hi: usize) -> Option<usize> {
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
