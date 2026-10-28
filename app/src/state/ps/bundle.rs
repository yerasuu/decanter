//! Release's bundle of the port ([`decanter_ps::os::linux::bundle`]), tried before any toolchain is looked
//! for, so a host without nix runs a release without downloading nix-portable. Tags from before
//! the bundles, and anything that stops one, leave the port to the nix cache as before.

use decanter_core::runner::Origin;
use decanter_nix::Toolchain;
use decanter_ps::bbport::Bbport;
use decanter_ps::channel::Channel;
use decanter_ps::os::linux::bundle;
use decanter_ps::os::linux::updater;
use decanter_ps::paths::Root;
use decanter_ps::port::Port;
use gpui::{AsyncApp, WeakEntity};

use super::Ps;
use super::channel;
use super::ports::{PortState, Stage, set_stage};

/// The bundle of the release the checkout is on, installed already or downloaded now, and
/// pointed at its own loader for wherever the library is. `None` sends the port down the nix
/// path: Testing, a port without bundles, a checkout on no release tag, a release without a
/// bundle, or a download, check or extraction that failed.
pub(super) async fn ready(this: &WeakEntity<Ps>, port: Port, root: &Root, cx: &mut AsyncApp) -> Option<Toolchain> {
    if port.release_downloads().is_none() || channel::load(this, port, root, cx).await != Channel::Release {
        return None;
    }
    let bbport = Bbport::new(root.clone());
    let tag = if bbport.is_built_with(&Toolchain::Bundle).await {
        bbport.bundle_tag().await
    } else {
        install(this, port, root, &bbport, cx).await
    };
    let Some(tag) = tag else {
        return None;
    };
    if let Err(err) = bundle::relocate(&bundle::dir(root, port, &tag)).await {
        tracing::warn!(port = port.name(), tag, %err, "port bundle can't run from here; using the nix path");
        set_stage(this, port, Stage::Resolving, cx);
        return None;
    }
    let _ = this.update(cx, |this, cx| {
        let state = this.ports.entry(port).or_insert_with(PortState::new);
        state.toolchain = Some((root.clone(), Toolchain::Bundle));
        state.origin = Some(Origin::Bundle);
        state.fell_back = false;
        cx.notify();
    });
    Some(Toolchain::Bundle)
}

async fn install(this: &WeakEntity<Ps>, port: Port, root: &Root, bbport: &Bbport, cx: &mut AsyncApp) -> Option<String> {
    let checkout = port.checkout(root);
    let Some(commit) = updater::upstream_commit(&checkout).await else {
        return None;
    };
    let Some(tag) = updater::release_tag(&checkout, &commit).await else {
        tracing::info!(
            port = port.name(),
            commit,
            "checkout on no release tag; no bundle for it"
        );
        return None;
    };
    let published = match bundle::published(port, &tag).await {
        Ok(Some(published)) => published,
        Ok(None) => {
            tracing::info!(port = port.name(), tag, "release without a bundle; using the nix cache");
            return None;
        }
        Err(err) => {
            tracing::warn!(port = port.name(), tag, %err, "could not look up the release's bundle; using the nix cache");
            return None;
        }
    };
    set_stage(this, port, Stage::DownloadingBundle(0, None), cx);
    let progress = this.clone();
    let mut async_cx = cx.clone();
    let installed = bundle::install(root, port, &tag, &published, move |received, total| {
        set_stage(
            &progress,
            port,
            Stage::DownloadingBundle(received, total),
            &mut async_cx,
        );
    })
    .await;
    set_stage(this, port, Stage::Resolving, cx);
    if let Err(err) = installed {
        tracing::warn!(port = port.name(), tag, %err, "port bundle not installed; using the nix cache");
        return None;
    }
    if let Err(err) = bbport.record_bundle(&tag).await {
        tracing::warn!(port = port.name(), tag, %err, "could not record the port bundle; using the nix cache");
        return None;
    }
    bundle::remove_others(root, port, &tag).await;
    tracing::info!(port = port.name(), commit, tag, "port runs from its release bundle");
    Some(tag)
}
