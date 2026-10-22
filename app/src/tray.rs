//! The tray decanter can close its window into: started where the
//! capability table offers one (see `platform` for the icon itself), and
//! its clicks carried back to GPUI's thread.

use gpui::App;

/// What the user asked for by clicking the icon or one of its menu items.
pub enum TrayCommand {
    Show,
    Quit,
}

/// GPUI has no tray of its own; the platform runs one and reports clicks
/// back through a channel this loop drains.
pub fn spawn(cx: &App) {
    let mut commands = crate::platform::spawn_tray();
    cx.spawn(async move |async_cx| {
        while let Some(command) = commands.recv().await {
            match command {
                TrayCommand::Show => async_cx.update(crate::views::show_main_window),
                // Games outlive decanter by design (see
                // `launch::stop_everything`'s doc) — quitting from the tray
                // is the same "just exit" a window close was before this
                // setting existed.
                TrayCommand::Quit => async_cx.update(|app| app.quit()),
            }
        }
    })
    .detach();
}
