//! A PS game as a Steam non-Steam shortcut that starts `decanter ps run` for its title, written
//! through `decanter_core::steam` as a GOG game's is. The entry is found again by its launch
//! options rather than a `steam_shortcuts` row, since a PS game has no account or storefront to key
//! one by; a shortcut the user deleted in Steam then reads as gone, as it is.
//!
//! Best-effort like the GOG export: a missing Steam, an unresolved Steam user or art the dump lacks
//! costs the shortcut or one image, logged, never the action that asked for it.

use std::path::{Path, PathBuf};

use decanter_core::steam;
use decanter_ps::library::Game;
use decanter_ps::title_id::TitleId;

const RUN: &str = "ps run --title";

/// `Some(exported)` when a menu can offer the shortcut: one is there already, or a Steam user to
/// add it for. Reads `shortcuts.vdf`, so it's for a menu being opened, not one being drawn.
pub fn exported(title_id: &str) -> Option<bool> {
    let steam_root = steam::steam_root()?;
    let user_id = steam::local_user_id(&steam_root)?;
    Some(own_appid(&steam_root, &user_id, title_id).is_some())
}

/// Adds the shortcut once a dump lands in the library, unless the Steam export is off in Ajustes,
/// as a GOG game's install does.
pub async fn export_if_enabled(game: Game) {
    let Ok(db) = crate::core::db() else {
        return;
    };
    // Writes into the real Steam user's own config files, so the Ajustes toggle that turns the GOG
    // export off turns this one off too.
    if !db.steam_automatic().is_ok_and(|automatic| automatic.export_shortcuts) {
        tracing::debug!(title_id = game.title_id, "steam export disabled in settings, skipping");
        return;
    }
    export(game).await;
}

/// Writes or updates the game's shortcut; the menu's "Add to Steam" comes here whatever the Ajustes
/// toggle says, since it is an explicit request.
pub async fn export(game: Game) {
    let title_id = match game.title_id.parse::<TitleId>() {
        Ok(title_id) => title_id,
        Err(err) => {
            tracing::warn!(%err, "PS game not exported to steam, its title id can't name a command");
            return;
        }
    };
    let Some(steam_root) = steam::steam_root() else {
        tracing::debug!(
            title_id = title_id.as_str(),
            "no Steam installation found, skipping Steam export"
        );
        return;
    };
    let Some(user_id) = steam::local_user_id(&steam_root) else {
        tracing::warn!(
            title_id = title_id.as_str(),
            "no local Steam user resolved, PS game not exported to steam"
        );
        return;
    };
    let Some(command) = crate::launcher::Launcher::ps(title_id.as_str()).and_then(|launcher| launcher.steam()) else {
        return;
    };
    let name = crate::state::ps_display_title(&game);
    let art = |file: &str| regular_file(&game.dir.join("sce_sys").join(file));
    let (icon, hero) = (art("icon0.png"), art("pic1.png"));
    let title = title_id.as_str().to_owned();
    let written = tokio::task::spawn_blocking(move || {
        let earlier = own_appid(&steam_root, &user_id, &title);
        let spec = steam::ShortcutSpec {
            name: &name,
            command: &command,
        };
        let Capsules { dir, portrait, wide } = icon.as_deref().map(Capsules::draw).unwrap_or_default();
        let artwork = steam::Artwork {
            icon,
            capsule: wide,
            portrait,
            hero,
            logo: None,
        };
        let appid = steam::sync(&steam_root, &user_id, &spec, &artwork);
        Capsules::clean(dir);
        let appid = appid?;
        // The appid hashes the program's path and the name, so a moved AppImage, an entry from
        // before shortcuts started the CLI through its loader, or a renamed dump would otherwise
        // leave the old entry beside the new one.
        if let Some(earlier) = earlier.filter(|earlier| *earlier != appid) {
            steam::remove(&steam_root, &user_id, earlier)?;
        }
        Ok::<_, std::io::Error>(appid)
    })
    .await;
    match written {
        Ok(Ok(appid)) => tracing::info!(title_id = title_id.as_str(), appid, "synced PS game steam shortcut"),
        Ok(Err(err)) => tracing::warn!(%err, "failed to export PS game steam shortcut"),
        Err(err) => tracing::warn!(%err, "PS game steam shortcut worker task panicked"),
    }
}

/// Removes the game's shortcut and its art; nothing to do when there is none.
pub async fn remove(title_id: String) {
    let Some(steam_root) = steam::steam_root() else {
        return;
    };
    let Some(user_id) = steam::local_user_id(&steam_root) else {
        return;
    };
    let removed = tokio::task::spawn_blocking(move || {
        let Some(appid) = own_appid(&steam_root, &user_id, &title_id) else {
            return Ok(None);
        };
        steam::remove(&steam_root, &user_id, appid).map(|()| Some(appid))
    })
    .await;
    match removed {
        Ok(Ok(Some(appid))) => tracing::info!(appid, "removed PS game steam shortcut"),
        Ok(Ok(None)) => tracing::debug!("no PS game steam shortcut to remove"),
        Ok(Err(err)) => tracing::warn!(%err, "failed to remove PS game steam shortcut"),
        Err(err) => tracing::warn!(%err, "PS game steam shortcut worker task panicked"),
    }
}

fn own_appid(steam_root: &Path, user_id: &str, title_id: &str) -> Option<i32> {
    steam::find(steam_root, user_id, |options| title_of(options) == Some(title_id))
        .inspect_err(|err| tracing::warn!(%err, "could not read steam shortcuts"))
        .ok()
        .flatten()
}

/// The title a shortcut's launch options run, `ps run --title <id>` at their end.
pub fn title_of(options: &str) -> Option<&str> {
    let (rest, title) = options.rsplit_once(' ')?;
    let before = rest.strip_suffix(RUN)?;
    (before.is_empty() || before.ends_with(' ')).then_some(title)
}

/// Art from the dump, which may hold anything: only a regular file there, never what a symlink
/// points to, is copied into Steam's grid folder.
fn regular_file(path: &Path) -> Option<PathBuf> {
    std::fs::symlink_metadata(path)
        .ok()
        .filter(std::fs::Metadata::is_file)
        .map(|_| path.to_owned())
}

/// Steam's sizes for the vertical and the wide capsule.
const PORTRAIT_SIZE: (u32, u32) = (600, 900);
const WIDE_SIZE: (u32, u32) = (920, 430);
/// A dump's `icon0.png` is 512 px square; anything far larger is not one.
const MAX_ICON_SIDE: u32 = 2048;
const MAX_DECODE_ALLOC: u64 = 64 * 1024 * 1024;

/// The capsules a dump has no art for, drawn from its square icon: the icon whole in the middle, on
/// a blurred, darkened copy of itself filling the rest. They wait in a private temp dir until
/// copied into Steam's grid.
#[derive(Default)]
pub struct Capsules {
    pub dir: Option<PathBuf>,
    pub portrait: Option<PathBuf>,
    pub wide: Option<PathBuf>,
}

impl Capsules {
    /// Whatever fails to decode or write leaves that capsule out, logged.
    pub fn draw(icon: &Path) -> Self {
        let Some(icon) = decode_icon(icon) else {
            return Self::default();
        };
        let dir = match decanter_core::temp::private_temp_dir("steam-artwork") {
            Ok(dir) => dir,
            Err(err) => {
                tracing::warn!(%err, "could not create a temp dir for the PS game's steam capsules");
                return Self::default();
            }
        };
        let draw = |(width, height), file: &str| {
            let path = dir.join(file);
            compose(&icon, width, height)
                .save_with_format(&path, image::ImageFormat::Png)
                .inspect_err(|err| tracing::warn!(path = %path.display(), %err, "could not write a steam capsule"))
                .ok()
                .map(|()| path)
        };
        Self {
            portrait: draw(PORTRAIT_SIZE, "portrait.png"),
            wide: draw(WIDE_SIZE, "wide.png"),
            dir: Some(dir),
        }
    }

    pub fn clean(dir: Option<PathBuf>) {
        if let Some(dir) = dir
            && let Err(err) = std::fs::remove_dir_all(&dir)
        {
            tracing::debug!(path = %dir.display(), %err, "could not clean up the steam capsules temp dir");
        }
    }
}

/// The icon as the dump holds it, which may be anything: decoded as a PNG under limits.
fn decode_icon(path: &Path) -> Option<image::RgbaImage> {
    let mut reader = image::ImageReader::open(path)
        .inspect_err(|err| tracing::warn!(path = %path.display(), %err, "could not open the PS game's icon"))
        .ok()?;
    reader.set_format(image::ImageFormat::Png);
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(MAX_ICON_SIDE);
    limits.max_image_height = Some(MAX_ICON_SIDE);
    limits.max_alloc = Some(MAX_DECODE_ALLOC);
    reader.limits(limits);
    reader
        .decode()
        .inspect_err(|err| tracing::warn!(path = %path.display(), %err, "the PS game's icon did not decode"))
        .ok()
        .map(image::DynamicImage::into_rgba8)
}

fn compose(icon: &image::RgbaImage, width: u32, height: u32) -> image::RgbaImage {
    use image::imageops::{self, FilterType};

    let cover = width.max(height);
    let background = imageops::resize(icon, cover, cover, FilterType::Triangle);
    let background = imageops::crop_imm(
        &background,
        cover.saturating_sub(width) / 2,
        cover.saturating_sub(height) / 2,
        width,
        height,
    )
    .to_image();
    let mut canvas = imageops::brighten(&imageops::fast_blur(&background, 24.0), -60);
    let side = width.min(height);
    let front = imageops::resize(icon, side, side, FilterType::Lanczos3);
    imageops::overlay(
        &mut canvas,
        &front,
        i64::from(width.saturating_sub(side) / 2),
        i64::from(height.saturating_sub(side) / 2),
    );
    canvas
}
