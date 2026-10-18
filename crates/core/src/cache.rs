//! Per-game shader caches, kept by the library folder a game lives in so
//! they never land in `$HOME` (drivers' and DXVK's default) and can sit on
//! a faster disk than the games themselves ([`crate::Db::library_cache`]).

use std::path::{Path, PathBuf};

use crate::Storefront;

/// Where `library_root` keeps caches unless pointed elsewhere: inside
/// itself, beside the rest of decanter's data for that folder.
#[must_use]
#[inline]
pub fn default_cache_root(library_root: &Path) -> PathBuf {
    crate::bootstrap::library_data_dir(library_root).join("cache")
}

/// The folder name a storefront's games are grouped under in a cache root.
#[must_use]
#[inline]
pub const fn cache_group(storefront: Storefront) -> &'static str {
    match storefront {
        Storefront::Unspecified => "external",
        Storefront::Steam => "steam",
        Storefront::EpicGames => "epic",
        Storefront::Gog => "gog",
        Storefront::AmazonGames => "amazon",
        Storefront::EaApp => "ea",
        Storefront::UbisoftConnect => "ubisoft",
        Storefront::BattleNet => "battlenet",
    }
}

/// `<cache_root>/<group>/<id>`; `None` for an id that isn't one plain path
/// component, since ids come from storefronts and dumps.
#[must_use]
#[inline]
pub fn game_cache_dir(cache_root: &Path, group: &str, id: &str) -> Option<PathBuf> {
    crate::library::safe_component(id).map(|component| cache_root.join(group).join(component))
}

/// The drivers' and translation layers' cache variables pointed into
/// `game_cache`, leaving out any the user already set in decanter's own
/// environment so a hand-made choice still wins.
#[inline]
pub fn shader_cache_env(game_cache: &Path) -> impl Iterator<Item = (&'static str, String)> + use<> {
    let dir = |name: &str| game_cache.join(name).to_string_lossy().into_owned();
    let nvidia = std::env::var_os("__GL_SHADER_DISK_CACHE_PATH").is_none();
    [
        ("MESA_SHADER_CACHE_DIR", dir("mesa")),
        ("__GL_SHADER_DISK_CACHE_PATH", dir("nvidia")),
        ("DXVK_STATE_CACHE_PATH", dir("dxvk")),
        ("VKD3D_SHADER_CACHE_PATH", dir("vkd3d")),
    ]
    .into_iter()
    // Only alongside a path decanter chose, so a cache the user set up keeps
    // its own settings: without SKIP_CLEANUP the driver trims the cache to
    // its size cap, evicting a large game's shaders between runs.
    .chain(
        nvidia
            .then_some([
                ("__GL_SHADER_DISK_CACHE", "1".to_owned()),
                ("__GL_SHADER_DISK_CACHE_SKIP_CLEANUP", "1".to_owned()),
            ])
            .into_iter()
            .flatten(),
    )
    .filter(|(name, _)| std::env::var_os(name).is_none())
}
