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

/// Keeps `command`'s windows from being taken for decanter's: the desktop names
/// the launching entry through `GIO_LAUNCHED_DESKTOP_FILE`, and pairs the first
/// window with the startup notification in `XDG_ACTIVATION_TOKEN` and
/// `DESKTOP_STARTUP_ID`, which the menu gave decanter for its own window.
#[inline]
pub fn strip_window_identity(command: &mut Command) {
    command
        .env_remove("GIO_LAUNCHED_DESKTOP_FILE")
        .env_remove("GIO_LAUNCHED_DESKTOP_FILE_PID")
        .env_remove("XDG_ACTIVATION_TOKEN")
        .env_remove("DESKTOP_STARTUP_ID");
}

/// Strips that environment from `command`.
///
/// `LD_LIBRARY_PATH`/`LD_PRELOAD` from a dev shell or another launcher
/// have broken Steam's own runtime before; `GLIBC_TUNABLES` is the malloc
/// arena cap AppRun sets, sized for a launcher, not a game; and a browser
/// started without EGL drivers loses GPU acceleration for its whole life.
/// `GIO_LAUNCHED_DESKTOP_FILE` names decanter's own desktop entry when the
/// menu opened it; inherited by Steam and its games, the desktop took their
/// windows for decanter's and showed decanter's icon on them.
///
/// The dev shell also points Vulkan at nix's own Mesa (`shell.nix`), which
/// only decanter's own nix-built binary can load. A game inside Steam's
/// runtime container gets no usable device from it — DXVK's `dxgi.dll`
/// crashed creating one, Arknights: Endfield's launcher with it — so a
/// driver list naming `/nix/store` is dropped and the game finds the
/// host's drivers itself. One set by hand to anything else still reaches it.
#[inline]
pub fn strip_launcher_env(command: &mut Command) {
    command
        .env_remove("LD_LIBRARY_PATH")
        .env_remove("LD_PRELOAD")
        .env_remove("GLIBC_TUNABLES");
    strip_window_identity(command);
    if std::env::var_os("__EGL_VENDOR_LIBRARY_FILENAMES").as_deref() == Some(OsStr::new(VULKAN_ONLY_EGL_VENDORS)) {
        command.env_remove("__EGL_VENDOR_LIBRARY_FILENAMES");
    }
    for var in ["VK_DRIVER_FILES", "VK_ICD_FILENAMES"] {
        if std::env::var_os(var).is_some_and(|value| names_nix_store(&value)) {
            command.env_remove(var);
        }
    }
}

/// Whether a `:`-separated path list has an entry under `/nix/store`.
fn names_nix_store(value: &OsStr) -> bool {
    value
        .to_string_lossy()
        .split(':')
        .any(|path| path.starts_with("/nix/store/"))
}
