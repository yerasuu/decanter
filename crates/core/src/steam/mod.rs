//! Exporting decanter's own installed games as real Steam "non-Steam
//! game" shortcuts, with grid artwork — so an install made through
//! decanter shows up in the actual Steam client too.
//!
//! Three parts: [`binary_vdf`] (the wire format `shortcuts.vdf` is
//! written in — distinct from `crate::vdf`'s text format), [`appid`] (the
//! id math Steam itself uses), and [`paths`] (finding the real Steam
//! installation and its logged-in user). [`export`] ties them together
//! into the actual merge-write. Nothing here touches the network — see
//! `decanter_gog::steam_export` for the piece that downloads artwork.

pub mod appid;
pub mod binary_vdf;
pub mod export;
pub mod paths;

pub use appid::{grid_appid, legacy_appid};
pub use export::{Artwork, ShortcutSpec, remove, sync};
pub use paths::{grid_dir, local_user_id, shortcuts_vdf_path, steam_root};
