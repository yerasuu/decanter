//! Release's half of keeping a port's checkout current
//! ([`decanter_ps::channel`]): on the tag the port's CI published last,
//! whose build [`super::ports`] then fetches instead of building.

use decanter_ps::channel::Channel;
use decanter_ps::os::linux::prebuilt::{self, Release};
use decanter_ps::os::linux::updater;
use decanter_ps::paths::Root;
use decanter_ps::port::Port;
use decanter_ps::process::JobKind;
use decanter_ps::remote::RemoteError;

use super::fallback;
use super::jobs;
use super::ports::Host;

/// Where Release has the checkout, for the Ports view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Track {
    OnRelease(Release),
    /// Published but not checked out yet: the checkout was busy, or git refused.
    Behind(Release),
    /// CI hasn't published a release yet: the default branch, built locally.
    NoRelease,
    /// The latest release couldn't be read or vouched for; the checkout stays where it was.
    Unknown(RemoteError),
}

/// Read again on every use, since Ajustes may have changed it, and kept for the Ports view.
pub(super) async fn load(host: &mut impl Host, port: Port, root: &Root) -> Channel {
    let for_read = root.clone();
    let channel = host.blocking(move || Channel::of(&for_read, port)).await;
    host.port(port, |state| {
        state.channel = Some(channel);
        if channel == Channel::Testing {
            state.track = None;
        }
    });
    channel
}

fn set_track(host: &mut impl Host, port: Port, track: Track) {
    host.port(port, |state| {
        // The default branch's check only means something while Release follows it.
        if track != Track::NoRelease {
            state.check = None;
        }
        state.track = Some(track);
    });
}

enum Found {
    OnIt,
    Elsewhere(Release),
    NoRelease,
    Unknown,
}

async fn find(host: &mut impl Host, port: Port, root: &Root) -> Found {
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
            set_track(host, port, Track::NoRelease);
            return Found::NoRelease;
        }
        Err(err) => {
            tracing::warn!(port = port.name(), cache, %err, "could not read the latest release; the checkout stays where it is");
            set_track(host, port, Track::Unknown(err.into()));
            return Found::Unknown;
        }
    };
    match updater::on_release(&port.checkout(root), port.upstream(), &release).await {
        Ok(true) => {
            set_track(host, port, Track::OnRelease(release));
            Found::OnIt
        }
        Ok(false) => {
            set_track(host, port, Track::Behind(release.clone()));
            Found::Elsewhere(release)
        }
        Err(err) => {
            tracing::warn!(port = port.name(), tag = release.tag, %err, "could not vouch for the latest release; the checkout stays where it is");
            set_track(host, port, Track::Unknown(err));
            Found::Unknown
        }
    }
}

/// The periodic check on Release. `false` when CI has no release yet, and the
/// default branch is followed instead, as on Testing. A release whose commit
/// failed to build before is left alone unless `retry_failed`.
pub(super) async fn follow(host: &mut impl Host, port: Port, root: &Root, retry_failed: bool) -> bool {
    let release = match find(host, port, root).await {
        Found::NoRelease => return false,
        Found::OnIt | Found::Unknown => return true,
        Found::Elsewhere(release) => release,
    };
    if !retry_failed && fallback::load(host, root, port).await.failed_at(&release.commit) {
        tracing::info!(
            port = port.name(),
            tag = release.tag,
            commit = release.commit,
            "latest release failed to build before; not moving onto it until a retry"
        );
        return true;
    }
    let command = updater::release_command(&port.checkout(root), &release);
    if host.move_checkout(port, root, command).await == Some(true) {
        tracing::info!(
            port = port.name(),
            tag = release.tag,
            commit = release.commit,
            "port on its latest release"
        );
        set_track(host, port, Track::OnRelease(release));
    }
    true
}

/// A clone lands on the default branch: onto the release before anything
/// builds it.
pub(super) async fn settle_clone(host: &mut impl Host, port: Port, root: &Root) {
    let Found::Elsewhere(release) = find(host, port, root).await else {
        return;
    };
    let command = updater::release_command(&port.checkout(root), &release);
    let facts = jobs::port_facts(root, port, None).await;
    let finished = host.run(root, port, JobKind::Update, facts, command).await;
    if finished.succeeded() {
        set_track(host, port, Track::OnRelease(release));
    }
}
