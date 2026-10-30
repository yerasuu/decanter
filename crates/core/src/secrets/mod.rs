//! Every secret decanter keeps: GOG sessions, shadNet passwords and tokens, the `SteamGridDB` key
//! (design: `docs/secure-credential-storage-design.md`, decision #1299).
//!
//! Kept in the OS keyring, never in the library: these are live credentials to someone's game
//! library and payment account, and the library travels with backups and syncs. A value only
//! ever lives in a [`Secret`], which prints as redacted and is zeroed when dropped; errors are
//! built from the keyring's error kind and message, never from what an entry holds; logs name
//! the [`SecretKind`] and the account id or NPID, never the value.
//!
//! Blocking on purpose. The Secret Service is a D-Bus round trip, called a handful of times per
//! session; async callers wrap these in `spawn_blocking` rather than this crate taking on a
//! runtime.

#![expect(
    clippy::implicit_return,
    reason = "directly contradicts needless_return from clippy::all, also enabled"
)]
#![expect(
    clippy::std_instead_of_core,
    clippy::std_instead_of_alloc,
    reason = "desktop app, no no_std target exists to justify core:: or alloc:: here"
)]
#![expect(
    clippy::large_stack_frames,
    reason = "a keyring_core::Error or a StoreError already costs more than the 512-byte threshold"
)]

mod file;
mod key;
mod native;
mod secret;
mod store;

pub use file::{FileError, FileOffer, file_path};
pub use key::{SecretKey, SecretKind, ShadnetSecret};
pub use secret::Secret;
pub use store::{BackendKind, Forgotten, Listing, Status, Store, StoreError};

static STORE: Store = Store::new();

/// The one store of this process.
#[inline]
#[must_use]
pub fn store() -> &'static Store {
    &STORE
}
