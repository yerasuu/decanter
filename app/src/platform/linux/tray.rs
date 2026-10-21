//! A from-scratch `StatusNotifierItem`. GPUI has no tray of its own, and
//! the usual crate for one (`tray-icon`/`muda`, what the Dioxus build used
//! off Linux) is broken here.
//!
//! That backend is broken on at least KDE Plasma, confirmed live against a
//! real session over `busctl`: it advertises `IconName` as a bare
//! filesystem path (`/run/user/…/tray-icon-*.png`) rather than a
//! theme-resolvable name — GTK-only hosts tolerate that, Qt/Plasma's
//! `QIcon::fromTheme` doesn't, so it falls back to a generic icon — and
//! every `dbusmenu` item comes back with empty properties, so Plasma has
//! no label to show and (verified by firing the `Event("clicked")` D-Bus
//! call directly at "Salir" and watching the process not exit) the click
//! never reaches `muda`'s activation signal at all. Both are inherent to
//! `tray-icon` 0.21.3 / `muda` 0.17.2's Linux/`libappindicator` bridge.
//!
//! `ksni` implements the `StatusNotifierItem` and `com.canonical.dbusmenu`
//! D-Bus interfaces directly — no GTK, no `libappindicator`, no bridging
//! layer to lose properties in. The icon goes over as a raw ARGB32 pixmap
//! (`icon_pixmap`), so there's no theme name to fail to resolve, and each
//! menu item's `activate` is a plain Rust closure `ksni` calls straight
//! from the incoming `Event` — no dbusmenu-gtk3 translation in between.

use std::sync::OnceLock;

use ksni::menu::StandardItem;
use ksni::{Handle, Icon, MenuItem, Tray, TrayMethods as _};
use rust_i18n::t;
use tokio::sync::mpsc::UnboundedReceiver;
use tokio::sync::mpsc::UnboundedSender;

use crate::tray::TrayCommand;

/// Kept for the life of the process so a language switch can have the menu
/// read again; nothing tears the tray down before the process exits.
static HANDLE: OnceLock<Handle<DecanterTray>> = OnceLock::new();

struct DecanterTray {
    commands: UnboundedSender<TrayCommand>,
}

impl Tray for DecanterTray {
    // Suffixed with the pid, same reason `tray-icon` derives its own id
    // from it: two decanter processes (as happens mid-restart, or while
    // testing) would otherwise fight over the same D-Bus name.
    fn id(&self) -> String {
        format!("decanter-{}", std::process::id())
    }

    fn title(&self) -> String {
        "Decanter".to_string()
    }

    fn icon_pixmap(&self) -> Vec<Icon> {
        decanter_icon().into_iter().collect()
    }

    // Left click, where a host implements it — Plasma's own native SNI
    // handling (not going through the appindicator translation the tray
    // itself no longer uses) is the one place this has a real chance of
    // working, unlike the icon-click path `tray-icon`'s AppIndicator
    // backend never delivered on Linux at all.
    fn activate(&mut self, _x: i32, _y: i32) {
        if self.commands.send(TrayCommand::Show).is_err() {
            tracing::warn!("tray command channel closed, could not forward show command");
        }
    }

    fn menu(&self) -> Vec<MenuItem<Self>> {
        vec![
            StandardItem {
                label: t!("chrome.tray.show").into_owned(),
                activate: Box::new(|tray: &mut Self| {
                    if tray.commands.send(TrayCommand::Show).is_err() {
                        tracing::warn!("tray command channel closed, could not forward show command");
                    }
                }),
                ..Default::default()
            }
            .into(),
            MenuItem::Separator,
            StandardItem {
                label: t!("chrome.tray.quit").into_owned(),
                activate: Box::new(|tray: &mut Self| {
                    if tray.commands.send(TrayCommand::Quit).is_err() {
                        tracing::warn!("tray command channel closed, could not forward quit command");
                    }
                }),
                ..Default::default()
            }
            .into(),
        ]
    }
}

/// Decodes the bundled favicon into the raw ARGB32-network-byte-order
/// pixel data `ksni::Icon` wants — `image`'s buffer is RGBA per pixel, so
/// each 4 bytes gets reordered to A,R,G,B rather than reinterpreted.
fn decanter_icon() -> Option<Icon> {
    let image = match image::load_from_memory(include_bytes!("../../../assets/favicon.ico")) {
        Ok(image) => image.into_rgba8(),
        Err(err) => {
            tracing::error!(error = %err, "bundled favicon.ico doesn't decode, tray shows no icon");
            return None;
        }
    };
    let (width, height) = image.dimensions();
    let data = image
        .into_raw()
        .as_chunks::<4>()
        .0
        .iter()
        .flat_map(|rgba| [rgba[3], rgba[0], rgba[1], rgba[2]])
        .collect();
    Some(Icon {
        width: width as i32,
        height: height as i32,
        data,
    })
}

/// Starts the tray as its own background task, off GPUI's event
/// loop entirely — `ksni` drives itself through its own `zbus` connection.
/// The receiver it returns is how the caller finds out what got clicked.
pub fn spawn_tray() -> UnboundedReceiver<TrayCommand> {
    let (commands, receiver) = tokio::sync::mpsc::unbounded_channel();
    tokio::spawn(async move {
        let tray = DecanterTray { commands };
        match tray.spawn().await {
            Ok(handle) => {
                tracing::info!("linux tray icon started");
                if HANDLE.set(handle).is_err() {
                    tracing::warn!("a second linux tray icon started, its menu won't follow a language switch");
                }
            }
            Err(err) => tracing::warn!(%err, "could not start the Linux tray icon"),
        }
    });
    receiver
}

/// Has the host read the menu again, after the language changed under it.
pub fn refresh_tray() {
    let Some(handle) = HANDLE.get().cloned() else {
        return;
    };
    tokio::spawn(async move {
        if handle.update(|_| {}).await.is_none() {
            tracing::warn!("linux tray icon already shut down, menu not refreshed");
        }
    });
}
