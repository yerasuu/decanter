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

use super::channel;
use super::ports::{Host, Stage, set_stage};

/// The bundle of the release the checkout is on, installed already or downloaded now, and
/// pointed at its own loader for wherever the library is. `None` sends the port down the nix
/// path: Testing, a port without bundles, a checkout on no release tag, a release without a
/// bundle, or a download, check or extraction that failed.
pub(super) async fn ready(host: &mut impl Host, port: Port, root: &Root) -> Option<Toolchain> {
    if port.release_downloads().is_none() || channel::load(host, port, root).await != Channel::Release {
        return None;
    }
    let bbport = Bbport::new(root.clone());
    let tag = if bbport.is_built_with(&Toolchain::Bundle).await {
        bbport.bundle_tag().await
    } else {
        install(host, port, root, &bbport).await
    };
    let Some(tag) = tag else {
        return None;
    };
    if let Err(err) = bundle::relocate(&bundle::dir(root, port, &tag)).await {
        tracing::warn!(port = port.name(), tag, %err, "port bundle can't run from here; using the nix path");
        set_stage(host, port, Stage::Resolving);
        return None;
    }
    host.port(port, |state| {
        state.toolchain = Some((root.clone(), Toolchain::Bundle));
        state.origin = Some(Origin::Bundle);
        state.fell_back = false;
    });
    Some(Toolchain::Bundle)
}

async fn install(host: &mut impl Host, port: Port, root: &Root, bbport: &Bbport) -> Option<String> {
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
    set_stage(host, port, Stage::DownloadingBundle(0, None));
    let installed = bundle::install(root, port, &tag, &published, |received, total| {
        set_stage(host, port, Stage::DownloadingBundle(received, total));
    })
    .await;
    set_stage(host, port, Stage::Resolving);
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
