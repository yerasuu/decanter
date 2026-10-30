//! Local state for decanter-sidecar and its UI: cached GOG library metadata
//! and achievement status, both scoped per account — see [`db`] for why.
//! Accounts group under a profile — see [`profiles`] and [`accounts`].

mod accounts;
pub mod bootstrap;
mod cache;
pub mod capabilities;
mod catalog;
mod collections;
mod db;
mod disk;
mod fingerprint;
mod game_tools;
mod launcher_env;
mod library;
pub mod migrate;
pub mod platform;
mod profiles;
pub mod runner;
mod scope;
pub mod secrets;
pub mod steam;
mod vdf;

pub use accounts::Account;
pub use cache::{cache_group, default_cache_root, game_cache_dir, shader_cache_env};
pub use catalog::{Game, GameSystem, InstallState, Storefront, UnknownStorefront};
pub use collections::Collection;
pub use db::{CachedAchievement, CachedGame, Db, Error, HookDiagnostic, Playtime, Result};
pub use disk::disk_usage;
pub use fingerprint::install_fingerprint;
pub use game_tools::{GameTool, gamemode_prefix, gamemoded, mangohud_env, mangohud_installed, mangohud_log_dir};
pub use launcher_env::{VULKAN_ONLY_EGL_VENDORS, strip_launcher_env};
pub use library::{GameArt, InstalledDlc, InstalledGame, Library, MoveProgress, copy_dir, directory_size, move_dir};
pub use profiles::Profile;
pub use scope::{application_id, game_key, own_scope};

/// How logs name this build: the workspace version and the commit it was built from.
pub const BUILD: &str = concat!(env!("CARGO_PKG_VERSION"), " (", env!("DECANTER_COMMIT"), ")");

/// This version's tag notes. Including them makes the build fail while `tag-changelog/v<version>.md` is missing, so a
/// version bump can't land without its notes.
pub const RELEASE_NOTES: &str = include_str!(concat!("../../../tag-changelog/v", env!("CARGO_PKG_VERSION"), ".md"));
