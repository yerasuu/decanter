//! Closing the game decanter started without leaving it: a controller
//! chord held for [`HOLD`], or the desktop's global shortcut, asks; a
//! notification says when and how to take it back; then the game stops
//! the way its Stop button stops it, so playtime, cloud saves and logs
//! wrap up as they always do.

mod notice;
mod pads;
mod shortcut;

use std::path::PathBuf;
use std::time::Duration;

use gpui::{App, AsyncApp, Global};
use tokio::sync::watch;
use tokio::time::Instant;

use crate::state::{JobKey, Ps};

pub const HOLD: Duration = Duration::from_secs(2);
/// Between the notice and the stop, to ask again and cancel it.
pub const GRACE: Duration = Duration::from_secs(5);

#[derive(Clone, PartialEq, Eq)]
pub enum Target {
    Pc { account_id: String, game_id: String },
    Ps(PathBuf),
}

#[derive(Clone)]
struct Running {
    target: Target,
    name: String,
}

struct Closing {
    /// Oldest first: a request closes the newest.
    running: Vec<Running>,
    listening: watch::Sender<bool>,
}

impl Global for Closing {}

/// While `target` runs, the chord and the shortcut can close it.
pub fn started(target: Target, name: String, cx: &mut App) {
    closing(cx).running.push(Running { target, name });
    sync(cx);
}

pub fn ended(target: &Target, cx: &mut App) {
    closing(cx).running.retain(|running| running.target != *target);
    sync(cx);
}

/// Listens only while a game runs and the setting is on: the pads stay
/// closed and the key stays free otherwise. Called again when the setting
/// changes.
pub fn sync(cx: &App) {
    let Some(closing) = cx.try_global::<Closing>() else {
        return;
    };
    let listening = !closing.running.is_empty() && enabled();
    closing.listening.send_if_modified(|current| {
        let changed = *current != listening;
        *current = listening;
        changed
    });
}

pub fn enabled() -> bool {
    crate::core::db()
        .and_then(|db| db.close_game_shortcut_enabled().map_err(crate::core::StoreError::Query))
        .inspect_err(
            |err| tracing::warn!(error = %err, "failed to read close_game_shortcut setting, defaulting to true"),
        )
        .unwrap_or(true)
}

fn closing(cx: &mut App) -> &mut Closing {
    if !cx.has_global::<Closing>() {
        let (listening, receiver) = watch::channel(false);
        cx.spawn(async move |cx| listen(receiver, cx).await).detach();
        cx.set_global(Closing {
            running: Vec::new(),
            listening,
        });
    }
    cx.global_mut::<Closing>()
}

struct Pending {
    running: Running,
    deadline: Instant,
    notice: Option<u32>,
}

enum Event {
    Asked(&'static str),
    Due,
    Stopped,
}

async fn listen(mut listening: watch::Receiver<bool>, cx: &AsyncApp) {
    while listening.wait_for(|listening| *listening).await.is_ok() {
        let mut watch = Watch::open().await;
        loop {
            let deadline = watch.pending.as_ref().map(|pending| pending.deadline);
            let event = tokio::select! {
                changed = listening.changed() => {
                    if changed.is_ok() && *listening.borrow() {
                        continue;
                    }
                    Event::Stopped
                }
                () = watch.pads.chord() => Event::Asked("gamepad"),
                () = shortcut::pressed(watch.shortcut.as_mut()) => Event::Asked("keyboard"),
                () = until(deadline) => Event::Due,
            };
            match event {
                Event::Asked(source) => watch.asked(source, cx).await,
                Event::Due => watch.due(cx).await,
                Event::Stopped => break,
            }
        }
        watch.close().await;
    }
}

/// Everything listened with while a game runs.
struct Watch {
    bus: Option<zbus::Connection>,
    shortcut: Option<shortcut::Shortcut>,
    pads: pads::Pads,
    pending: Option<Pending>,
}

impl Watch {
    async fn open() -> Self {
        tracing::info!("listening for the close-game chord and shortcut");
        let bus = zbus::Connection::session()
            .await
            .inspect_err(|err| tracing::warn!(error = %err, "no session bus, closing a game has no shortcut or notice"))
            .ok();
        let shortcut = match &bus {
            Some(bus) => shortcut::Shortcut::register(bus).await,
            None => None,
        };
        Self {
            bus,
            shortcut,
            pads: pads::Pads::new(),
            pending: None,
        }
    }

    /// Asking again while the notice is up takes it back.
    async fn asked(&mut self, source: &'static str, cx: &AsyncApp) {
        if let Some(cancelled) = self.pending.take() {
            tracing::info!(source, name = %cancelled.running.name, "closing the game cancelled");
            notice::cancelled(self.bus.as_ref(), cancelled.notice, &cancelled.running.name).await;
            return;
        }
        let Some(running) = cx.read_global(|closing: &Closing, _| closing.running.last().cloned()) else {
            return;
        };
        tracing::info!(source, name = %running.name, grace_secs = GRACE.as_secs(), "closing the game asked for");
        let notice = notice::closing(self.bus.as_ref(), &running.name, self.shortcut.is_some()).await;
        self.pending = Some(Pending {
            running,
            deadline: Instant::now() + GRACE,
            notice,
        });
    }

    async fn due(&mut self, cx: &AsyncApp) {
        if let Some(due) = self.pending.take() {
            notice::close(self.bus.as_ref(), due.notice).await;
            cx.update(|cx| stop(due.running, cx));
        }
    }

    async fn close(self) {
        if let Some(pending) = self.pending {
            notice::close(self.bus.as_ref(), pending.notice).await;
        }
        if let Some(shortcut) = self.shortcut {
            shortcut.release().await;
        }
        tracing::info!("stopped listening for the close-game chord and shortcut");
    }
}

/// What the game's own Stop button does. Skipped when the game already
/// ended on its own during the notice.
fn stop(running: Running, cx: &mut App) {
    let still_running = cx
        .global::<Closing>()
        .running
        .iter()
        .any(|other| other.target == running.target);
    if !still_running {
        return;
    }
    tracing::info!(name = %running.name, "closing the game");
    match running.target {
        Target::Pc { account_id, game_id } => {
            cx.spawn(async move |_| crate::launch::stop(account_id, game_id).await)
                .detach();
        }
        Target::Ps(dir) => {
            let ps = Ps::get(cx);
            ps.update(cx, |ps, cx| ps.stop(&JobKey::Game(dir), cx));
        }
    }
}

async fn until(deadline: Option<Instant>) {
    match deadline {
        Some(deadline) => tokio::time::sleep_until(deadline).await,
        None => std::future::pending().await,
    }
}
