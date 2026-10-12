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

use ksni::menu::StandardItem;
use ksni::{Icon, MenuItem, Tray, TrayMethods as _};
use rust_i18n::t;
use tokio::sync::mpsc::UnboundedReceiver;
use tokio::sync::mpsc::UnboundedSender;

/// What the user asked for by clicking the icon or one of its menu items.
pub enum TrayCommand {
    Show,
    Quit,
}

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
        vec![decanter_icon()]
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
fn decanter_icon() -> Icon {
    let image = image::load_from_memory(include_bytes!("../assets/favicon.ico"))
        .expect("bundled favicon.ico decodes")
        .into_rgba8();
    let (width, height) = image.dimensions();
    let data = image
        .into_raw()
        .as_chunks::<4>()
        .0
        .iter()
        .flat_map(|rgba| [rgba[3], rgba[0], rgba[1], rgba[2]])
        .collect();
    Icon {
        width: width as i32,
        height: height as i32,
        data,
    }
}

/// Starts the tray as its own background task, off GPUI's event
/// loop entirely — `ksni` drives itself through its own `zbus` connection.
/// The returned receiver is how the caller finds out what got clicked;
/// the `ksni::Handle` itself is deliberately leaked; nothing here ever
/// needs to tear the tray back down before the process exits.
pub fn spawn() -> UnboundedReceiver<TrayCommand> {
    let (commands, receiver) = tokio::sync::mpsc::unbounded_channel();
    tokio::spawn(async move {
        let tray = DecanterTray { commands };
        match tray.spawn().await {
            Ok(handle) => {
                tracing::info!("linux tray icon started");
                std::mem::forget(handle);
            }
            Err(err) => tracing::warn!(%err, "could not start the Linux tray icon"),
        }
    });
    receiver
}
