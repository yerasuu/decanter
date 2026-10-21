//! The keyboard side, as a Plasma global shortcut registered with
//! `KGlobalAccel` itself rather than through the XDG `GlobalShortcuts` portal:
//! the portal wants a reverse-DNS app id that decanter's `decanter`
//! desktop entry can't give it, and on Plasma it confirms the shortcut in
//! a dialog, which would come up over the game it was registered for.
//! The user can rebind it in System Settings > Shortcuts.

use futures::StreamExt as _;
use rust_i18n::t;
use zbus::proxy::SignalStream;
use zbus::zvariant::OwnedObjectPath;
use zbus::{Connection, Proxy};

const SERVICE: &str = "org.kde.kglobalaccel";
const COMPONENT: &str = "decanter";
const ACTION: &str = "close-game";
/// Ctrl+Shift+F4 the way Qt encodes a key combination: `Qt::CTRL |
/// Qt::SHIFT | Qt::Key_F4`. Home is psport's own close prompt.
const DEFAULT_KEY: i32 = 0x0400_0000 | 0x0200_0000 | 0x0100_0033;
/// `KGlobalAccel::SetShortcutFlag`: the action is live, and leaving out
/// `NoAutoloading` lets a key picked in System Settings win.
const SET_PRESENT: u32 = 2;
const IS_DEFAULT: u32 = 8;

pub struct Shortcut {
    kglobalaccel: Proxy<'static>,
    pressed: SignalStream<'static>,
    /// The name it was registered under: releasing it under another, after
    /// a language switch, would leave it active.
    name: String,
}

impl Shortcut {
    /// `None` where nothing provides `KGlobalAccel` (any desktop but Plasma):
    /// the controller chord still works there.
    pub async fn register(bus: &Connection) -> Option<Self> {
        match Self::try_register(bus).await {
            Ok(shortcut) => {
                tracing::info!(component = COMPONENT, action = ACTION, "close-game shortcut registered");
                Some(shortcut)
            }
            Err(err) => {
                tracing::info!(error = %err, "no close-game shortcut, KGlobalAccel unavailable");
                None
            }
        }
    }

    async fn try_register(bus: &Connection) -> zbus::Result<Self> {
        let name = t!("closing.shortcut_name").into_owned();
        let id = action_id(&name);
        // Always four ints, unused ones 0: KGlobalAccel reads a key sequence
        // as exactly four (QKeySequence's longest) and aborts on fewer,
        // taking KWin, which hosts it on Plasma 6, down with it.
        let keys = vec![(vec![DEFAULT_KEY, 0, 0, 0],)];
        let kglobalaccel = crate::achv_try!(Proxy::new(bus, SERVICE, "/kglobalaccel", "org.kde.KGlobalAccel").await);
        crate::achv_try!(kglobalaccel.call_method("doRegister", &(id.as_slice(),)).await);
        crate::achv_try!(
            kglobalaccel
                .call_method("setShortcutKeys", &(id.as_slice(), &keys, SET_PRESENT))
                .await
        );
        crate::achv_try!(
            kglobalaccel
                .call_method("setShortcutKeys", &(id.as_slice(), &keys, IS_DEFAULT))
                .await
        );
        let path: OwnedObjectPath = crate::achv_try!(kglobalaccel.call("getComponent", &(COMPONENT,)).await);
        let component = crate::achv_try!(Proxy::new(bus, SERVICE, path, "org.kde.kglobalaccel.Component").await);
        let pressed = crate::achv_try!(component.receive_signal("globalShortcutPressed").await);
        Ok(Self {
            kglobalaccel,
            pressed,
            name,
        })
    }

    /// Hands the key back to the rest of the desktop while no game runs.
    pub async fn release(self) {
        let id = action_id(&self.name);
        if let Err(err) = self.kglobalaccel.call_method("setInactive", &(id.as_slice(),)).await {
            tracing::warn!(error = %err, "could not release the close-game shortcut");
        }
    }
}

/// Never resolves without a shortcut, so it can sit in a `select!` beside
/// the chord. Cancel-safe.
pub async fn pressed(shortcut: Option<&mut Shortcut>) {
    let Some(shortcut) = shortcut else {
        return std::future::pending().await;
    };
    while let Some(message) = shortcut.pressed.next().await {
        match message.body().deserialize::<(String, String, i64)>() {
            Ok((_, action, _)) if action == ACTION => return,
            Ok(_) => {}
            Err(err) => tracing::warn!(error = %err, "unreadable globalShortcutPressed signal"),
        }
    }
    tracing::warn!("KGlobalAccel's signals stopped, the close-game shortcut no longer works");
    std::future::pending::<()>().await;
}

/// `KGlobalAccel`'s action id: component and action, then the names System
/// Settings shows for them.
const fn action_id(name: &str) -> [&str; 4] {
    [COMPONENT, ACTION, "Decanter", name]
}
