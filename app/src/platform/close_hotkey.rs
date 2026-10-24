//! Closing a game on Windows and macOS: a global shortcut through
//! `global-hotkey`, registered only while a game runs, as the Plasma one
//! is, with a system notification between the press and the stop. No pads
//! yet.
//!
//! The manager must be made, and its keys registered, on the thread that
//! runs the event loop: `closing::listen` runs on GPUI's main thread, so
//! everything here does.

use std::rc::Rc;
use std::sync::{LazyLock, Once};

use global_hotkey::hotkey::{Code, HotKey, Modifiers};
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState};
use gpui::{App, AsyncApp, Global, Platform, SystemNotification};
use rust_i18n::t;
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

/// One tag for every close notice, so the cancellation replaces the warning
/// and the stop takes whichever is up away.
const NOTICE_TAG: &str = "decanter-close-game";

/// The notices post through the platform itself, not through `App`: its
/// borrow would last across the toast's COM call, whose nested message pump
/// runs the window's frame callbacks, and those then fail to borrow `App`.
struct NoticeCenter(Rc<dyn Platform>);

impl Global for NoticeCenter {}

pub fn keep_platform(platform: &Rc<dyn Platform>, cx: &mut App) {
    cx.set_global(NoticeCenter(Rc::clone(platform)));
}

pub struct CloseWatch {
    shortcut: Option<(GlobalHotKeyManager, HotKey)>,
    presses: watch::Receiver<()>,
    platform: Option<Rc<dyn Platform>>,
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
    pub async fn open(cx: &AsyncApp) -> Self {
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
        let platform = cx.try_read_global(|center: &NoticeCenter, _| Rc::clone(&center.0));
        if platform.is_none() {
            tracing::warn!("no platform to post the close-game notices through, they won't show");
        }
        Self {
            shortcut,
            presses,
            platform,
        }
    }

    /// Never resolves without a shortcut. Cancel-safe.
    pub async fn asked(&mut self) -> &'static str {
        if self.shortcut.is_some() && self.presses.changed().await.is_ok() {
            return "keyboard";
        }
        std::future::pending().await
    }

    pub fn notices(&self) -> Notices {
        Notices {
            platform: self.platform.clone(),
        }
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

/// The notification between asking to close the game and its stop, posted
/// from GPUI's main thread, which the platform's notification center needs.
#[derive(Clone)]
pub struct Notices {
    platform: Option<Rc<dyn Platform>>,
}

#[expect(
    clippy::unused_async,
    clippy::unused_async_trait_impl,
    reason = "the same signatures as the Linux notices, which await the bus"
)]
#[expect(
    clippy::future_not_send,
    reason = "the platform stays on the main thread, which the notification center needs"
)]
impl Notices {
    /// The tag names the notification, so there is no id to hand back.
    pub async fn closing(self, name: &str) -> Option<u32> {
        let seconds = crate::closing::GRACE.as_secs();
        self.show(
            &t!("closing.notice.title", name = name),
            &t!("closing.notice.body_keyboard", seconds = seconds),
        );
        None
    }

    pub async fn cancelled(self, _replaces: Option<u32>, name: &str) {
        self.show(&t!("closing.notice.cancelled", name = name), "");
    }

    pub async fn close(self, _id: Option<u32>) {
        if let Some(platform) = &self.platform {
            platform.dismiss_system_notification(NOTICE_TAG);
        }
    }

    fn show(&self, title: &str, body: &str) {
        let notification = SystemNotification {
            tag: NOTICE_TAG.into(),
            title: title.to_owned().into(),
            body: body.to_owned().into(),
            actions: Vec::new(),
        };
        if let Some(platform) = &self.platform {
            platform.show_system_notification(notification);
        }
    }
}
