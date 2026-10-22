//! The tray on Windows and macOS, through `tray-icon`: its native backends
//! there have none of the Linux one's faults (see `linux/tray.rs`). Both
//! want the icon made on the thread that runs the event loop, GPUI's main
//! thread, which is where `tray::spawn` calls this from.

use std::cell::OnceCell;

use rust_i18n::t;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};
use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};

use crate::tray::TrayCommand;

const SHOW: &str = "show";
const QUIT: &str = "quit";

struct Tray {
    /// Held only so the icon stays: it's removed once this is dropped.
    _icon: TrayIcon,
    show: MenuItem,
    quit: MenuItem,
}

thread_local! {
    /// Never leaves the main thread, which neither the icon nor its items
    /// may leave, and lives as long as the process.
    static TRAY: OnceCell<Tray> = const { OnceCell::new() };
}

/// The receiver is closed from the start when the icon can't be made.
pub fn spawn_tray() -> UnboundedReceiver<TrayCommand> {
    let (commands, receiver) = unbounded_channel();
    let show = MenuItem::with_id(SHOW, t!("chrome.tray.show"), true, None);
    let quit = MenuItem::with_id(QUIT, t!("chrome.tray.quit"), true, None);
    let menu = match Menu::with_items(&[&show, &PredefinedMenuItem::separator(), &quit]) {
        Ok(menu) => menu,
        Err(err) => {
            tracing::warn!(%err, "could not build the tray menu, no tray icon");
            return receiver;
        }
    };
    // A left click shows the window, as on Linux; the menu is on the right.
    let mut builder = TrayIconBuilder::new()
        .with_tooltip("Decanter")
        .with_menu(Box::new(menu))
        .with_menu_on_left_click(false);
    if let Some(icon) = decanter_icon() {
        builder = builder.with_icon(icon);
    }
    let icon = match builder.build() {
        Ok(icon) => icon,
        Err(err) => {
            tracing::warn!(%err, "could not start the tray icon");
            return receiver;
        }
    };
    forward_events(commands);
    let started = TRAY.with(|tray| {
        tray.set(Tray {
            _icon: icon,
            show,
            quit,
        })
        .is_ok()
    });
    if started {
        tracing::info!("tray icon started");
    } else {
        tracing::warn!("a second tray icon was asked for, the first one stays");
    }
    receiver
}

/// Both handlers run on the main thread, inside the event loop's dispatch.
fn forward_events(commands: UnboundedSender<TrayCommand>) {
    let clicks = commands.clone();
    TrayIconEvent::set_event_handler(Some(move |event| {
        if let TrayIconEvent::Click {
            button: MouseButton::Left,
            button_state: MouseButtonState::Up,
            ..
        } = event
        {
            send(&clicks, TrayCommand::Show);
        }
    }));
    MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
        if event.id == SHOW {
            send(&commands, TrayCommand::Show);
        } else if event.id == QUIT {
            send(&commands, TrayCommand::Quit);
        } else {
            tracing::warn!(id = ?event.id, "click on an unknown tray menu item");
        }
    }));
}

fn send(commands: &UnboundedSender<TrayCommand>, command: TrayCommand) {
    if commands.send(command).is_err() {
        tracing::warn!("tray command channel closed, click dropped");
    }
}

fn decanter_icon() -> Option<Icon> {
    let image = image::load_from_memory(include_bytes!("../../assets/favicon.ico"))
        .inspect_err(|err| tracing::error!(%err, "bundled favicon.ico doesn't decode, tray shows no icon"))
        .ok()?
        .into_rgba8();
    let (width, height) = image.dimensions();
    Icon::from_rgba(image.into_raw(), width, height)
        .inspect_err(|err| tracing::error!(%err, "tray icon rejected the bundled favicon"))
        .ok()
}

/// Relabels the menu after the language changed under it.
pub fn refresh_tray() {
    TRAY.with(|cell| {
        if let Some(tray) = cell.get() {
            tray.show.set_text(t!("chrome.tray.show"));
            tray.quit.set_text(t!("chrome.tray.quit"));
        }
    });
}
