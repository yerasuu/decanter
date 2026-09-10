//! Local state for achv-sidecar and its UI: cached GOG library metadata
//! and achievement status, both scoped per profile — see [`db`] for why.

pub mod adopt;
mod catalog;
mod collections;
mod credentials;
mod db;
mod disk;
mod library;
mod profiles;
mod vdf;

pub use catalog::{Game, InstallState, Storefront, UnknownStorefront};
pub use collections::Collection;
pub use credentials::{KeyringError, delete as delete_session, load as load_session, save as save_session};
pub use db::{CachedAchievement, CachedGame, Db, Error, Playtime, Result, default_db_path};
pub use disk::disk_usage;
pub use library::{InstalledDlc, InstalledGame, Library};
pub use profiles::Profile;

/// Where achv keeps everything it writes: the cache database, the game
/// library, and anything else that isn't a per-OS keyring entry.
#[must_use]
#[inline]
pub fn data_dir() -> std::path::PathBuf {
    dirs::data_dir().expect("no data dir for this platform").join("achv")
}
