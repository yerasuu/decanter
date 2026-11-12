//! Exporting decanter's own installed games as real Steam "non-Steam
//! game" shortcuts, with grid artwork — so an install made through
//! decanter shows up in the actual Steam client too.
//!
//! Four parts: [`binary_vdf`] (the wire format `shortcuts.vdf` is written
//! in — distinct from `crate::vdf`'s text format), [`appid`] (the id math
//! Steam itself uses), [`paths`] (finding the real Steam installation and
//! its logged-in user), and [`process`] (starting/stopping the real Steam
//! client, needed because it holds its own in-memory copy of
//! `shortcuts.vdf` while running). [`library`] reads back what Steam
//! already has installed and the user's own shortcuts, [`managed`] what it
//! keeps for one of its apps (Proton, launch options, branch). [`folders`] reads
//! and merges Steam's list of library folders. [`export`] ties
//! the write side together into the actual merge-write, staging a change
//! behind Steam's back when it's running rather than writing straight
//! into a file it would just overwrite again. Nothing here touches the network — see
//! `decanter_gog::steam_export` for the piece that downloads artwork.

pub mod appid;
pub mod binary_vdf;
pub mod export;
pub mod folders;
pub mod library;
pub mod managed;
pub mod paths;
pub mod process;

pub use appid::{grid_appid, legacy_appid, legacy_grid_id};
pub use export::{
    ApplyError, Artwork, Command, GridArt, Own, ShortcutSpec, apply_pending_and_restart_steam, find,
    has_pending_changes, own_entries, refresh_art, remove, retarget, sync,
};
pub use folders::{LibraryFolder, Registration, library_folders, register};
pub use library::{InstalledApp, Shortcut, SteamArt, installed_apps, non_steam_shortcuts};
pub use managed::{CompatTool, Managed, managed};
pub use paths::{grid_dir, local_user_id, login_names, shortcuts_vdf_path, steam_root};
