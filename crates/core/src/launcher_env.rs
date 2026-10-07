//! Environment decanter runs under that's meant for decanter alone, and
//! has to be kept from whatever it starts — games, Steam, a browser.

use std::ffi::OsStr;
use std::process::Command;

/// What the AppImage's AppRun points `__EGL_VENDOR_LIBRARY_FILENAMES` at
/// (see scripts/appimage) so libglvnd finds no EGL driver: GPUI renders
/// through Vulkan, and Mesa's GL stack loading beside it for nothing cost
/// ~45 MB. Only this value is stripped from children, so an override the
/// user set themselves still reaches them.
pub const VULKAN_ONLY_EGL_VENDORS: &str = "/nonexistent/decanter-vulkan-only.json";

/// Strips that environment from `command`.
///
/// `LD_LIBRARY_PATH`/`LD_PRELOAD` from a dev shell or another launcher
/// have broken Steam's own runtime before; `GLIBC_TUNABLES` is the malloc
/// arena cap AppRun sets, sized for a launcher, not a game; and a browser
/// started without EGL drivers loses GPU acceleration for its whole life.
#[inline]
pub fn strip_launcher_env(command: &mut Command) {
    command
        .env_remove("LD_LIBRARY_PATH")
        .env_remove("LD_PRELOAD")
        .env_remove("GLIBC_TUNABLES");
    if std::env::var_os("__EGL_VENDOR_LIBRARY_FILENAMES").as_deref() == Some(OsStr::new(VULKAN_ONLY_EGL_VENDORS)) {
        command.env_remove("__EGL_VENDOR_LIBRARY_FILENAMES");
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::OsStr;
    use std::process::Command;

    use super::strip_launcher_env;

    #[test]
    fn strips_what_broke_steam_and_the_allocator_cap() {
        let mut command = Command::new("steam");
        strip_launcher_env(&mut command);
        let removed: Vec<&OsStr> = command
            .get_envs()
            .filter(|(_, value)| value.is_none())
            .map(|(key, _)| key)
            .collect();
        for var in ["LD_LIBRARY_PATH", "LD_PRELOAD", "GLIBC_TUNABLES"] {
            assert!(removed.contains(&OsStr::new(var)), "{var} not stripped");
        }
    }
}
