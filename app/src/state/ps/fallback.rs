//! A port whose build fails once its checkout moved goes back to the last commit it built at
//! ([`decanter_ps::os::linux::builds`]) and runs from there, rather than staying unusable until upstream
//! fixes it. The commit that broke is remembered, so the automatic update doesn't move onto it
//! again; [`Ps::retry_update`] does.

use std::path::PathBuf;

use decanter_nix::Toolchain;
use decanter_ps::failures;
use decanter_ps::os::linux::builds::Record;
use decanter_ps::os::linux::toolchain;
use decanter_ps::os::linux::updater;
use decanter_ps::paths::{self, Root};
use decanter_ps::port::Port;
use decanter_ps::process::JobKind;
use gpui::Context;

use super::Ps;
use super::jobs;
use super::ports::{self, Host, PortError, Stage, set_stage};

/// The port runs on `good` because `failed` didn't build, for the Ports view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fallback {
    pub good: String,
    pub failed: String,
    /// The failed build's full log.
    pub log: Option<PathBuf>,
}

fn short(commit: &str) -> String {
    commit.get(..9).unwrap_or(commit).to_owned()
}

pub(super) async fn load(host: &impl Host, root: &Root, port: Port) -> Record {
    let root = root.clone();
    host.blocking(move || Record::load(&root, port)).await
}

impl Ps {
    /// The update the automatic one now skips, since its commit failed to build: tried again,
    /// built, and fallen back from again if it still fails.
    pub fn retry_update(&mut self, port: Port, cx: &mut Context<Self>) {
        let root = paths::active();
        cx.spawn(async move |this, cx| ports::check_once(&this, port, &root, true, cx).await)
            .detach();
    }
}

/// After a build of `port` failed: back on the last good commit and built there, when the
/// checkout has one, sits elsewhere, and holds nothing local. `Err` is what the port's stage
/// then says.
pub(super) async fn recover(host: &mut impl Host, port: Port, root: &Root) -> Result<Toolchain, PortError> {
    let checkout = port.checkout(root);
    let record = load(host, root, port).await;
    let Some(good) = record.good else {
        tracing::info!(
            port = port.name(),
            "build failed and the port never built before; nothing to fall back to"
        );
        return Err(PortError::Build);
    };
    let Some(head) = updater::upstream_commit(&checkout).await else {
        tracing::warn!(
            port = port.name(),
            good,
            "build failed on a checkout with local changes or commits; not putting it back on the last good commit"
        );
        return Err(PortError::BuildNotRolledBack(short(&good)));
    };
    if head == good {
        tracing::info!(
            port = port.name(),
            commit = head,
            "build failed at the last commit that built; nothing to fall back to"
        );
        return Err(PortError::Build);
    }
    if !updater::has_commit(&checkout, &good).await {
        tracing::warn!(
            port = port.name(),
            good,
            "the last commit that built is gone from the checkout; not falling back"
        );
        return Err(PortError::Build);
    }
    let failed_log = host.last_log(port);
    let for_record = (root.clone(), head.clone());
    let recorded = host
        .blocking(move || Record::failed(&for_record.0, port, &for_record.1, failed_log))
        .await;
    if let Err(err) = recorded {
        // Falls back all the same, since an unrecorded failure only costs another try on the update's next round.
        tracing::warn!(port = port.name(), failed = head, %err, "could not record the commit that failed to build");
    }
    tracing::warn!(
        port = port.name(),
        failed = head,
        good,
        "build failed; putting the checkout back on the last commit that built"
    );

    set_stage(host, port, Stage::RollingBack);
    let facts = jobs::port_facts(root, port, None).await;
    let finished = host
        .run(
            root,
            port,
            JobKind::Rollback,
            facts,
            updater::rollback_command(&checkout, &good),
        )
        .await;
    if !finished.succeeded() {
        return Err(PortError::Rollback(short(&good)));
    }
    let toolchain = match Toolchain::resolve(&root.toolchain(), &toolchain::recipe(port), |_, _| {}).await {
        Ok(toolchain) => toolchain,
        Err(err) => return Err(PortError::Toolchain(err)),
    };
    host.port(port, |state| state.toolchain = Some((root.clone(), toolchain.clone())));
    match ports::build(host, port, root, toolchain).await {
        Ok(toolchain) => {
            tracing::info!(
                port = port.name(),
                commit = good,
                failed = head,
                "port runs on the last commit that built"
            );
            Ok(toolchain)
        }
        Err(err) => {
            tracing::warn!(port = port.name(), commit = good, %err, "the last commit that built failed to build again");
            Err(err)
        }
    }
}

/// Once preparing or rebuilding `port` ended: when it `built`, records the commit it built at and
/// shows whether that is a fallback; a port that didn't build runs on nothing.
pub(super) async fn settle(host: &mut impl Host, port: Port, root: &Root, built: bool) {
    let fallback = if built { built_at(host, port, root).await } else { None };
    host.port(port, |state| state.fallback = fallback);
}

async fn built_at(host: &impl Host, port: Port, root: &Root) -> Option<Fallback> {
    let checkout = port.checkout(root);
    if let Some(commit) = updater::upstream_commit(&checkout).await {
        let for_record = root.clone();
        let recorded = host.blocking(move || Record::built(&for_record, port, &commit)).await;
        if let Err(err) = recorded {
            tracing::warn!(port = port.name(), %err, "could not record the commit the port built at");
        }
    }
    let record = load(host, root, port).await;
    let head = failures::full_commit_of(&checkout).await;
    match (&record.good, &record.failed) {
        (Some(good), Some(failed)) if *good == head => Some(Fallback {
            good: short(good),
            failed: short(&failed.commit),
            log: record.failed_log(root),
        }),
        _ => None,
    }
}
