//! The desktop notification between asking to close the game and its stop.
//! Every call is best-effort: without a notification server (gamescope's
//! session, say) the game still closes on time.

use std::collections::HashMap;

use rust_i18n::t;
use zbus::Connection;
use zbus::zvariant::Value;

/// Critical, so Plasma shows it over a fullscreen game and through Do Not
/// Disturb.
const CRITICAL: u8 = 2;
const NORMAL: u8 = 1;
const CANCELLED_TIMEOUT_MS: i32 = 4000;

pub async fn closing(bus: Option<&Connection>, name: &str, with_shortcut: bool) -> Option<u32> {
    let seconds = crate::closing::GRACE.as_secs();
    let body = if with_shortcut {
        t!("closing.notice.body", seconds = seconds)
    } else {
        t!("closing.notice.body_pad", seconds = seconds)
    };
    notify(bus, None, &t!("closing.notice.title", name = name), &body, CRITICAL, 0).await
}

pub async fn cancelled(bus: Option<&Connection>, replaces: Option<u32>, name: &str) {
    let summary = t!("closing.notice.cancelled", name = name);
    notify(bus, replaces, &summary, "", NORMAL, CANCELLED_TIMEOUT_MS).await;
}

/// A critical notification stays until dismissed otherwise.
pub async fn close(bus: Option<&Connection>, id: Option<u32>) {
    let (Some(bus), Some(id)) = (bus, id) else {
        return;
    };
    let closed = bus
        .call_method(
            Some("org.freedesktop.Notifications"),
            "/org/freedesktop/Notifications",
            Some("org.freedesktop.Notifications"),
            "CloseNotification",
            &(id,),
        )
        .await;
    if let Err(err) = closed {
        tracing::debug!(id, error = %err, "could not close the close-game notification");
    }
}

async fn notify(
    bus: Option<&Connection>,
    replaces: Option<u32>,
    summary: &str,
    body: &str,
    urgency: u8,
    timeout_ms: i32,
) -> Option<u32> {
    let Some(bus) = bus else {
        return None;
    };
    let hints = HashMap::from([
        ("urgency", Value::from(urgency)),
        ("desktop-entry", Value::from("decanter")),
    ]);
    let actions: &[&str] = &[];
    let shown = bus
        .call_method(
            Some("org.freedesktop.Notifications"),
            "/org/freedesktop/Notifications",
            Some("org.freedesktop.Notifications"),
            "Notify",
            &(
                "Decanter",
                replaces.unwrap_or(0),
                "decanter",
                summary,
                body,
                actions,
                hints,
                timeout_ms,
            ),
        )
        .await
        .and_then(|reply| reply.body().deserialize::<u32>());
    match shown {
        Ok(id) => Some(id),
        Err(err) => {
            tracing::warn!(error = %err, "could not show the close-game notification");
            None
        }
    }
}
