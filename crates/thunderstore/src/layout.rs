// SPDX-License-Identifier: GPL-3.0-or-later
//! Where a package's files go in the game, worked out from the import's preview of its zip.
//!
//! The package format (its wiki page, read by the analysis) puts `icon.png`, `README.md`, `manifest.json` and
//! `CHANGELOG.md` at the zip's root: Thunderstore's metadata, left out of the mod so that two
//! packages never fight over them. It says nothing of where the rest goes. For `BepInEx` games
//! decanter follows what the analysis reads of `r2modman`'s convention (a manager's convention,
//! secondary and unverified, section 2): a package laid out from the game's root (its `BepInEx/`
//! folder there, as the `BepInEx` pack is) goes to the root; `plugins/`, `patchers/`, `config/` and
//! `monomod/` go under `BepInEx/`; a package with none of them goes to
//! `BepInEx/plugins/<Team-Name>/`. A package that mixes those folders with anything else matches
//! no rule and is imported as any archive, the preview saying so.

use decanter_mods::{ArchivePreview, ModOrigin, ModPath};

use crate::id::{DOMAIN, PackageId};
use crate::listing::PackageVersion;
use crate::settings::Layout;
use crate::{PLATFORM, Result};

pub const LEAVE_OUT: [&str; 4] = ["icon.png", "README.md", "manifest.json", "CHANGELOG.md"];

const BEPINEX: &str = "BepInEx";
const PLUGINS: &str = "plugins";
const FOLDERS: [&str; 4] = [PLUGINS, "patchers", "config", "monomod"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum PlacementKind {
    /// Laid out from the game's root, `BepInEx/` included.
    GameRoot,
    /// Its `BepInEx` folders under the game's `BepInEx/`.
    BepInEx,
    /// No `BepInEx` folder: all of it in its own folder of `BepInEx/plugins/`.
    PluginFolder,
    /// `BepInEx` folders mixed with other files: no rule fits, placed as any archive.
    Unrecognised,
    /// The game's layout is plain: placed as any archive.
    Plain,
}

#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct Placement {
    /// Where the files go, `None` for the game's root.
    pub target: Option<ModPath>,
    pub kind: PlacementKind,
}

/// Where `package`'s files go under `layout`, from `preview` read with [`LEAVE_OUT`].
///
/// # Errors
///
/// When a `BepInEx` target can't be a mod path, which a checked package name never makes.
#[inline]
pub fn place(preview: &ArchivePreview, layout: Layout, package: &PackageId) -> Result<Placement> {
    let as_archive = |kind| Placement {
        target: preview.target().cloned(),
        kind,
    };
    if layout == Layout::Plain {
        return Ok(as_archive(PlacementKind::Plain));
    }
    let is_folder = |name: &str| FOLDERS.iter().any(|folder| folder.eq_ignore_ascii_case(name));
    let tops = preview.top_folders();
    let stripped = preview.stripped().map(ModPath::as_str);
    // The import takes off a single wrapping folder the game has no folder of, which a
    // `plugins/`-only or `BepInEx/`-only package is: that folder is then where it goes.
    if let Some(wrapper) = stripped {
        if wrapper.eq_ignore_ascii_case(BEPINEX) {
            return placed(BEPINEX, PlacementKind::BepInEx);
        }
        if is_folder(wrapper) {
            return placed(&format!("{BEPINEX}/{}", wrapper.to_lowercase()), PlacementKind::BepInEx);
        }
    }
    if tops.iter().any(|name| name.eq_ignore_ascii_case(BEPINEX)) {
        return Ok(Placement {
            target: None,
            kind: PlacementKind::GameRoot,
        });
    }
    let known = tops.iter().filter(|name| is_folder(name)).count();
    if known == 0 {
        return placed(&format!("{BEPINEX}/{PLUGINS}/{package}"), PlacementKind::PluginFolder);
    }
    if known == tops.len() && !preview.has_top_files() {
        return placed(BEPINEX, PlacementKind::BepInEx);
    }
    Ok(as_archive(PlacementKind::Unrecognised))
}

fn placed(target: &str, kind: PlacementKind) -> Result<Placement> {
    Ok(Placement {
        target: Some(ModPath::parse(target)?),
        kind,
    })
}

/// What the mod's `mod.toml` records of where it came from: the package, the version and the
/// host its download was asked from.
///
/// # Errors
///
/// When a field can't be an origin's, which checked names never make.
#[inline]
pub fn origin(package: &PackageId, version: &PackageVersion) -> Result<ModOrigin> {
    let host = version.download_url.host_str().unwrap_or(DOMAIN);
    Ok(ModOrigin::new(
        PLATFORM,
        &package.to_string(),
        &version.number.to_string(),
        host,
    )?)
}
