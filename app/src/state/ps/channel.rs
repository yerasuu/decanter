//! Release's half of keeping a port's checkout current
//! ([`decanter_ps::channel`]): on the tag the port's CI published last,
//! whose build [`super::ports`] then fetches instead of building.

use decanter_ps::channel::Channel;
use decanter_ps::paths::Root;
use decanter_ps::port::Port;
use decanter_ps::prebuilt::{self, Release};
use decanter_ps::process::JobKind;
use decanter_ps::updater;
use gpui::{AppContext as _, AsyncApp, WeakEntity};

use super::Ps;
use super::jobs::{self, JobKey};
use super::ports::{self, PortState};

/// Where Release has the checkout, for the Ports view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Track {
    OnRelease(Release),
    /// Published but not checked out yet: the checkout was busy, or git refused.
    Behind(Release),
    /// CI hasn't published a release yet: the default branch, built locally.
    NoRelease,
    /// The latest release couldn't be read or vouched for; the checkout stays where it was.
    Unknown(String),
}

/// Read again on every use, since Ajustes may have changed it, and kept for the Ports view.
pub(super) async fn load(this: &WeakEntity<Ps>, port: Port, root: &Root, cx: &mut AsyncApp) -> Channel {
    let for_read = root.clone();
    let channel = cx.background_spawn(async move { Channel::of(&for_read, port) }).await;
    let _ = this.update(cx, |this, cx| {
        let state = this.ports.entry(port).or_insert_with(PortState::new);
        state.channel = Some(channel);
        if channel == Channel::Testing {
            state.track = None;
        }
        cx.notify();
    });
    channel
}

fn set_track(this: &WeakEntity<Ps>, port: Port, track: Track, cx: &mut AsyncApp) {
    let _ = this.update(cx, |this, cx| {
        let state = this.ports.entry(port).or_insert_with(PortState::new);
        // The default branch's check only means something while Release follows it.
        if track != Track::NoRelease {
            state.check = None;
        }
        state.track = Some(track);
        cx.notify();
    });
}

enum Found {
    OnIt,
    Elsewhere(Release),
    NoRelease,
    Unknown,
}

async fn find(this: &WeakEntity<Ps>, port: Port, root: &Root, cx: &mut AsyncApp) -> Found {
    let Some(cache) = prebuilt::cache(port) else {
        return Found::NoRelease;
    };
    let release = match prebuilt::latest_release(&cache).await {
        Ok(Some(release)) => release,
        Ok(None) => {
            tracing::info!(
                port = port.name(),
                cache,
                "no release published yet; following the default branch and building locally"
            );
            set_track(this, port, Track::NoRelease, cx);
            return Found::NoRelease;
        }
        Err(err) => {
            tracing::warn!(port = port.name(), cache, %err, "could not read the latest release; the checkout stays where it is");
            set_track(this, port, Track::Unknown(err.to_string()), cx);
            return Found::Unknown;
        }
    };
    match updater::on_release(&port.checkout(root), port.upstream(), &release).await {
        Ok(true) => {
            set_track(this, port, Track::OnRelease(release), cx);
            Found::OnIt
        }
        Ok(false) => {
            set_track(this, port, Track::Behind(release.clone()), cx);
            Found::Elsewhere(release)
        }
        Err(err) => {
            tracing::warn!(port = port.name(), tag = release.tag, %err, "could not vouch for the latest release; the checkout stays where it is");
            set_track(this, port, Track::Unknown(err), cx);
            Found::Unknown
        }
    }
}

/// The periodic check on Release. `false` when CI has no release yet, and the
/// default branch is followed instead, as on Testing.
pub(super) async fn follow(this: &WeakEntity<Ps>, port: Port, root: &Root, cx: &mut AsyncApp) -> bool {
    let release = match find(this, port, root, cx).await {
        Found::NoRelease => return false,
        Found::OnIt | Found::Unknown => return true,
        Found::Elsewhere(release) => release,
    };
    let command = updater::release_command(&port.checkout(root), &release);
    if let Some(moving) = ports::start_update(this, port, root, command, cx)
        && moving.await
    {
        tracing::info!(
            port = port.name(),
            tag = release.tag,
            commit = release.commit,
            "port on its latest release"
        );
        set_track(this, port, Track::OnRelease(release), cx);
    }
    true
}

/// A clone lands on the default branch: onto the release before anything
/// builds it.
pub(super) async fn settle_clone(this: &WeakEntity<Ps>, port: Port, root: &Root, cx: &mut AsyncApp) {
    let Found::Elsewhere(release) = find(this, port, root, cx).await else {
        return;
    };
    let command = updater::release_command(&port.checkout(root), &release);
    let facts = jobs::port_facts(root, port, None).await;
    let finished = jobs::run(
        this,
        root,
        JobKey::Port(port),
        JobKind::Update,
        facts,
        command,
        None,
        cx,
    )
    .await;
    if finished.succeeded() {
        set_track(this, port, Track::OnRelease(release), cx);
    }
}
