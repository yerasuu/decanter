//! Local state for decanter-sidecar and its UI: cached GOG library metadata
//! and achievement status, both scoped per account — see [`db`] for why.
//! Accounts group under a profile — see [`profiles`] and [`accounts`].

mod accounts;
mod catalog;
mod collections;
mod credentials;
mod db;
mod disk;
mod library;
mod profiles;
pub mod steam;
mod vdf;

pub use accounts::Account;
pub use catalog::{Game, InstallState, Storefront, UnknownStorefront};
pub use collections::Collection;
pub use credentials::{KeyringError, delete as delete_session, load as load_session, save as save_session};
pub use db::{CachedAchievement, CachedGame, Db, Error, Playtime, Result, default_db_path};
pub use disk::disk_usage;
pub use library::{InstalledDlc, InstalledGame, Library, MoveProgress};
pub use profiles::Profile;

/// Where decanter keeps everything it writes: the cache database, the game
/// library, and anything else that isn't a per-OS keyring entry.
#[must_use]
#[inline]
pub fn data_dir() -> std::path::PathBuf {
    dirs::data_dir()
        .expect("no data dir for this platform")
        .join("decanter")
}
