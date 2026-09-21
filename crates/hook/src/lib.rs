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
//!
//! Split by concern: [`ffi`] (raw Win32 calls and the real DLL), [`log`]
//! (the JSONL trail), [`abi_scan`] (finding `SetAchievement`'s vtable
//! slot), [`hook`] (patching it in), [`exports`] (every forwarded export,
//! which is also this DLL's actual public surface).

// The crate has to be called `Galaxy64` for the built DLL to be the name
// the game imports; that is not a snake-case identifier and cannot be.
#![allow(non_snake_case)]

mod abi_scan;
mod exports;
mod ffi;
mod hook;
mod log;

