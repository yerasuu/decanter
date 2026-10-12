//! Local state for decanter-sidecar and its UI: cached GOG library metadata
//! and achievement status, both scoped per account — see [`db`] for why.
//! Accounts group under a profile — see [`profiles`] and [`accounts`].

mod accounts;
pub mod bootstrap;
mod catalog;
mod collections;
mod credentials;
mod db;
mod disk;
mod launcher_env;
mod library;
pub mod migrate;
mod profiles;
mod scope;
pub mod steam;
mod vdf;

pub use accounts::Account;
pub use catalog::{Game, InstallState, Storefront, UnknownStorefront};
pub use collections::Collection;
pub use credentials::{KeyringError, delete as delete_session, load as load_session, save as save_session};
pub use db::{CachedAchievement, CachedGame, Db, Error, HookDiagnostic, Playtime, Result};
pub use disk::disk_usage;
pub use launcher_env::{VULKAN_ONLY_EGL_VENDORS, strip_launcher_env};
pub use library::{GameArt, InstalledDlc, InstalledGame, Library, MoveProgress, copy_dir, directory_size, move_dir};
pub use profiles::Profile;
pub use scope::{application_id, game_key, own_scope};

/// How logs name this build: the workspace version and the commit it was built from.
pub const BUILD: &str = concat!(env!("CARGO_PKG_VERSION"), " (", env!("DECANTER_COMMIT"), ")");
