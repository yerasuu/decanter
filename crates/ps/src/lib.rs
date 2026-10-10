//! PS4 and PS5 games run through native ports: bbport (Bloodborne) and
//! AnyPS5 (a relinker for PS5 executables).
//!
//! Nothing here touches the Steam-shaped library in `decanter_core`: a PS
//! game is the user's own dump folder, read where it is, and what
//! decanter generates for it lives under its own data directory. No GPUI
//! either — the app owns the views, this crate owns the formats and the
//! processes.

#![expect(
    clippy::implicit_return,
    reason = "directly contradicts needless_return from clippy::all, also enabled"
)]
#![expect(
    clippy::large_stack_frames,
    reason = "the binding defaults table and the SFO test fixture cost more than the 512-byte threshold"
)]

pub mod anyps5;
pub mod bbport;
pub mod binds;
pub mod failures;
mod files;
pub mod keys;
pub mod layouts;
pub mod library;
pub mod mapping;
pub mod paths;
pub mod port;
pub mod process;
pub mod status;
pub mod toolchain;
pub mod updater;
