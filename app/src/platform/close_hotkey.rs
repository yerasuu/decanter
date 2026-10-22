//! Closing a game on Windows and macOS: a global shortcut through
//! `global-hotkey`, registered only while a game runs, as the Plasma one
//! is. No pads and no notice yet.
//!
//! The manager must be made, and its keys registered, on the thread that
//! runs the event loop: `closing::listen` runs on GPUI's main thread, so
//! everything here does.

use std::sync::{LazyLock, Once};

use global_hotkey::hotkey::{Code, HotKey, Modifiers};
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState};
use tokio::sync::watch;

/// What Ajustes says the switch for it does: there's only the keyboard here.
pub const CLOSE_GAME_LABEL: &str = "settings.general.close_game.enabled_keyboard";
pub const CLOSE_GAME_HINT: &str = "settings.general.close_game.hint_keyboard";

/// Ctrl+Shift+F4, Plasma's default too. Home is psport's own close prompt.
fn close_game() -> HotKey {
    HotKey::new(Some(Modifiers::CONTROL | Modifiers::SHIFT), Code::F4)
}

/// Bumped on every press. `global-hotkey` takes a single handler for the
/// whole process, so it feeds this and each watch subscribes, which also
/// leaves behind presses from before it opened.
static PRESSES: LazyLock<watch::Sender<()>> = LazyLock::new(|| watch::channel(()).0);
static HANDLER: Once = Once::new();

pub struct CloseWatch {
    shortcut: Option<(GlobalHotKeyManager, HotKey)>,
    presses: watch::Receiver<()>,
}

#[expect(
    clippy::unused_async,
    clippy::unused_async_trait_impl,
    clippy::unused_self,
    reason = "the same signatures as the Linux watch, which awaits the bus and the pads"
)]
#[expect(
    clippy::future_not_send,
    reason = "the manager stays on the main thread that made it, as Win32 and AppKit require"
)]
impl CloseWatch {
    pub async fn open() -> Self {
        HANDLER.call_once(|| {
            let id = close_game().id();
            GlobalHotKeyEvent::set_event_handler(Some(move |event: GlobalHotKeyEvent| {
                if event.id == id && event.state == HotKeyState::Pressed {
                    PRESSES.send_replace(());
                }
            }));
        });
        let presses = PRESSES.subscribe();
        let hotkey = close_game();
        let shortcut = GlobalHotKeyManager::new()
            .and_then(|manager| manager.register(hotkey).map(|()| (manager, hotkey)))
            .inspect(|_| tracing::info!(hotkey = %hotkey.into_string(), "close-game shortcut registered"))
            .inspect_err(|err| tracing::warn!(error = %err, "no close-game shortcut, it could not be registered"))
            .ok();
        Self { shortcut, presses }
    }

    /// Never resolves without a shortcut. Cancel-safe.
    pub async fn asked(&mut self) -> &'static str {
        if self.shortcut.is_some() && self.presses.changed().await.is_ok() {
            return "keyboard";
        }
        std::future::pending().await
    }

    pub const fn notices(&self) -> Notices {
        Notices
    }

    /// Hands the key back to the rest of the desktop while no game runs.
    pub async fn release(self) {
        if let Some((manager, hotkey)) = self.shortcut
            && let Err(err) = manager.unregister(hotkey)
        {
            tracing::warn!(error = %err, "could not release the close-game shortcut");
        }
    }
}

/// Nothing to show a notice with here yet.
#[derive(Clone, Copy)]
pub struct Notices;

#[expect(
    clippy::unused_async,
    clippy::unused_async_trait_impl,
    reason = "the same signatures as the Linux notices, which await the bus"
)]
impl Notices {
    pub async fn closing(self, _name: &str) -> Option<u32> {
        None
    }

    pub async fn cancelled(self, _replaces: Option<u32>, _name: &str) {}

    pub async fn close(self, _id: Option<u32>) {}
}
