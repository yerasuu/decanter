//! Installing a package: its plan (it and its dependencies, those already installed left out), the
//! downloads into the game's staging folder with a progress line and a cancel, each archive read by
//! the mods import with where its files go, and only after *Añadir* the imports themselves, in the
//! plan's order so the chosen package goes last and wins.

use core::sync::atomic::{AtomicU8, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use decanter_mods::{ArchivePreview, GameMods, ModPath, Progress};
use decanter_thunderstore::{
    Client, LEAVE_OUT, Layout, Listing, PackageId, Placement, PlacementKind, Plan, Skipped, Staging, Transfer,
    VersionNumber,
};
use gpui::{Context, Div, div, prelude::*, px};
use rust_i18n::t;

use super::{ModsTab, failure_text};
use crate::i18n::Text;
use crate::theme;
use crate::widgets::{ButtonKind, button, progress_bar};

const TICK: Duration = Duration::from_millis(200);
const SHOWN_OVERRIDES: usize = 5;

const PLANNING: u8 = 0;
const DOWNLOADING: u8 = 1;
const READING: u8 = 2;
const IMPORTING: u8 = 3;

/// What runs off the UI, shared with the panel that draws it and cancels it.
#[derive(Default)]
pub(in crate::views::library::properties::mods) struct Work {
    phase: AtomicU8,
    transfer: Transfer,
    progress: Progress,
    current: AtomicUsize,
    count: AtomicUsize,
}

impl Work {
    fn cancel(&self) {
        self.transfer.cancel();
        self.progress.cancel();
    }
}

pub(in crate::views::library::properties::mods) enum Install {
    Working(Arc<Work>),
    Ready(Ready),
}

impl Install {
    /// Stops what runs, or drops what was downloaded, off the UI thread since it deletes files.
    pub(super) fn discard(self) {
        match self {
            Self::Working(work) => {
                work.cancel();
                tracing::info!("thunderstore install cancel asked");
            }
            Self::Ready(ready) => {
                tracing::info!(
                    packages = ready.items.len(),
                    "thunderstore install dismissed, nothing added"
                );
                tokio::task::spawn_blocking(move || drop(ready));
            }
        }
    }
}

pub(in crate::views::library::properties::mods) struct Ready {
    items: Vec<ReadyItem>,
    skipped: Vec<Skipped>,
    /// Holds the downloads until the imports are done; dropping it deletes them.
    staging: Staging,
}

struct ReadyItem {
    package: PackageId,
    version: VersionNumber,
    is_dependency: bool,
    preview: ArchivePreview,
    placement: Placement,
}

/// How the imports ended: how many mods were added, and how many were already the game's.
struct Added {
    new: usize,
    duplicates: usize,
    failure: Option<decanter_mods::Error>,
}

impl ModsTab {
    pub(super) fn install_package(&mut self, package: PackageId, version: VersionNumber, cx: &mut Context<Self>) {
        let Some(mods) = self.mods.clone() else {
            return;
        };
        let Some(browser) = self.thunderstore.as_mut() else {
            return;
        };
        let (Some(listing), Some(settings)) = (browser.listing.clone(), browser.settings.as_ref()) else {
            return;
        };
        let layout = settings.layout;
        let client = browser.client.clone();
        let work = Arc::new(Work::default());
        browser.install = Some(Install::Working(Arc::clone(&work)));
        browser.failure = None;
        self.error = None;
        self.notice = None;
        tracing::info!(package = %package, version = %version, "thunderstore install started");
        Self::follow_install(cx);
        cx.notify();
        cx.spawn(async move |this, cx| {
            let request = Request {
                client,
                mods,
                listing,
                package,
                version,
                layout,
            };
            let shared = Arc::clone(&work);
            let prepared = tokio::spawn(async move { prepare(request, shared).await }).await;
            let _ = this.update(cx, |this, cx| {
                let Some(browser) = this.thunderstore.as_mut() else {
                    return;
                };
                browser.install = None;
                match prepared {
                    Ok(Ok(ready)) => browser.install = Some(Install::Ready(ready)),
                    Ok(Err(err)) => {
                        tracing::warn!(%err, retryable = err.is_retryable(), "thunderstore install not prepared");
                        let text = failure_text(&err);
                        if text.is_or_wraps("mods.thunderstore.cancelled") {
                            this.notice = Some(text);
                        } else {
                            browser.failure = Some(super::Failure { text, retry: None });
                        }
                    }
                    Err(err) => {
                        tracing::error!(%err, "thunderstore install task stopped");
                        this.error = Some(Text::verbatim(err));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn add_packages(&mut self, cx: &mut Context<Self>) {
        let Some(mods) = self.mods.clone() else {
            return;
        };
        let Some(browser) = self.thunderstore.as_mut() else {
            return;
        };
        let Some(Install::Ready(ready)) = browser.install.take() else {
            return;
        };
        let account_id = self.target.account_id.clone();
        let work = Arc::new(Work::default());
        work.phase.store(IMPORTING, Ordering::Relaxed);
        work.count.store(ready.items.len(), Ordering::Relaxed);
        browser.install = Some(Install::Working(Arc::clone(&work)));
        Self::follow_install(cx);
        cx.notify();
        cx.spawn(async move |this, cx| {
            let added = tokio::task::spawn_blocking(move || import_all(&mods, ready, &account_id, &work)).await;
            let _ = this.update(cx, |this, cx| {
                if let Some(browser) = this.thunderstore.as_mut() {
                    browser.install = None;
                }
                match added {
                    Ok(added) => {
                        this.notice = Some(
                            Text::new("mods.thunderstore.added")
                                .arg("count", added.new)
                                .arg("duplicates", added.duplicates),
                        );
                        if let Some(err) = added.failure {
                            this.error = Some(if matches!(err, decanter_mods::Error::Cancelled) {
                                Text::new("mods.thunderstore.cancelled")
                            } else {
                                Text::new("mods.thunderstore.import_failed").arg("reason", err)
                            });
                        }
                    }
                    Err(err) => {
                        tracing::error!(%err, "thunderstore import task stopped");
                        this.error = Some(Text::verbatim(err));
                    }
                }
                this.reload(cx);
            });
        })
        .detach();
    }

    /// Redraws the progress line until the work ends.
    fn follow_install(cx: &Context<Self>) {
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(TICK).await;
                let working = this.update(cx, |this, cx| {
                    cx.notify();
                    this.thunderstore
                        .as_ref()
                        .is_some_and(|browser| matches!(browser.install, Some(Install::Working(_))))
                });
                if !matches!(working, Ok(true)) {
                    break;
                }
            }
        })
        .detach();
    }
}

pub(super) fn panel(install: &Install, cx: &Context<ModsTab>) -> Div {
    match install {
        Install::Working(work) => working_panel(work, cx),
        Install::Ready(ready) => ready_panel(ready, cx),
    }
}

struct Request {
    client: Client,
    mods: GameMods,
    listing: Arc<Listing>,
    package: PackageId,
    version: VersionNumber,
    layout: Layout,
}

/// The plan, its downloads and each archive's preview, all before anything is added.
async fn prepare(request: Request, work: Arc<Work>) -> decanter_thunderstore::Result<Ready> {
    let Request {
        client,
        mods,
        listing,
        package,
        version,
        layout,
    } = request;
    let staged_mods = mods.clone();
    let planned = tokio::task::spawn_blocking(move || {
        let installed = decanter_thunderstore::installed(&staged_mods.list()?);
        let plan = decanter_thunderstore::plan(&listing, &package, version, &installed)?;
        let staging = Staging::new(&staged_mods)?;
        Ok::<_, decanter_thunderstore::Error>((plan, staging))
    })
    .await
    .map_err(|err| decanter_thunderstore::Error::Io {
        path: mods.dir().to_path_buf(),
        source: std::io::Error::other(err),
    })?;
    let (plan, staging) = planned?;
    work.count.store(plan.items.len(), Ordering::Relaxed);
    work.phase.store(DOWNLOADING, Ordering::Relaxed);
    let downloaded = decanter_thunderstore::download_plan(&client, &staging, &plan, &work.transfer).await?;
    work.phase.store(READING, Ordering::Relaxed);
    let dir = mods.dir().to_path_buf();
    tokio::task::spawn_blocking(move || read_all(&mods, plan, &downloaded, staging, layout, &work))
        .await
        .map_err(|err| decanter_thunderstore::Error::Io {
            path: dir,
            source: std::io::Error::other(err),
        })?
}

fn read_all(
    mods: &GameMods,
    plan: Plan,
    downloaded: &[decanter_thunderstore::Downloaded],
    staging: Staging,
    layout: Layout,
    work: &Work,
) -> decanter_thunderstore::Result<Ready> {
    let mut items = Vec::with_capacity(plan.items.len());
    for (index, (item, file)) in plan.items.into_iter().zip(downloaded).enumerate() {
        work.current.store(index, Ordering::Relaxed);
        let preview = mods.preview_package(&file.path, &work.progress, &LEAVE_OUT)?;
        let placement = decanter_thunderstore::place(&preview, layout, &item.package)?;
        let origin = decanter_thunderstore::origin(&item.package, &item.version)?;
        tracing::info!(
            package = %item.package,
            version = %item.version.number,
            placement = ?placement.kind,
            target = ?placement.target.as_ref().map(ModPath::as_str),
            files = preview.files(),
            "thunderstore package read"
        );
        items.push(ReadyItem {
            version: item.version.number,
            is_dependency: item.is_dependency,
            preview: preview.with_origin(origin),
            placement,
            package: item.package,
        });
    }
    Ok(Ready {
        items,
        skipped: plan.skipped,
        staging,
    })
}

/// Each package in turn, skipping one the game already has from the same archive; a failure
/// stops there, leaving the ones before it added, since each is a whole mod of its own.
fn import_all(mods: &GameMods, ready: Ready, account_id: &str, work: &Work) -> Added {
    let mut added = Added {
        new: 0,
        duplicates: 0,
        failure: None,
    };
    for (index, item) in ready.items.iter().enumerate() {
        work.current.store(index, Ordering::Relaxed);
        if let Some((id, _)) = item.preview.duplicate_of() {
            tracing::info!(package = %item.package, mod_id = %id, "thunderstore package already a mod of the game");
            added.duplicates = added.duplicates.saturating_add(1);
            continue;
        }
        let name = item.package.display_name();
        match mods.import_archive(
            &item.preview,
            &name,
            item.placement.target.clone(),
            account_id,
            &work.progress,
        ) {
            Ok(_) => added.new = added.new.saturating_add(1),
            Err(err) => {
                tracing::warn!(package = %item.package, %err, "thunderstore package not added");
                added.failure = Some(err);
                break;
            }
        }
    }
    tracing::info!(
        added = added.new,
        duplicates = added.duplicates,
        failed = added.failure.is_some(),
        "thunderstore install finished"
    );
    drop(ready.staging);
    added
}

fn working_panel(work: &Arc<Work>, cx: &Context<ModsTab>) -> Div {
    let current = work.current.load(Ordering::Relaxed).saturating_add(1);
    let count = work.count.load(Ordering::Relaxed);
    let stopping = work.progress.is_cancelled() || work.transfer.is_cancelled();
    let (snapshot, text) = match work.phase.load(Ordering::Relaxed) {
        PLANNING => (None, t!("mods.thunderstore.planning")),
        DOWNLOADING => {
            let snapshot = work.transfer.snapshot();
            let (done, total) = snapshot.unwrap_or_default();
            (
                snapshot,
                t!(
                    "mods.thunderstore.downloading",
                    current = current,
                    count = count,
                    done = crate::format::bytes(done),
                    total = crate::format::bytes(total)
                ),
            )
        }
        READING => (
            work.progress.snapshot(),
            t!("mods.thunderstore.reading", current = current, count = count),
        ),
        _ => (
            work.progress.snapshot(),
            t!("mods.thunderstore.importing", current = current, count = count),
        ),
    };
    let text = if stopping { t!("mods.archive.cancelling") } else { text };
    let cancel = Arc::clone(work);
    div()
        .flex()
        .flex_col()
        .gap(px(6.))
        .child(theme::muted(text))
        .child(progress_bar(
            "ts-progress",
            snapshot.map(|(done, total)| done as f64 / total as f64),
            6.,
        ))
        .child(div().flex().child(
            button("ts-stop", ButtonKind::Secondary, t!("common.cancel"), stopping).when(!stopping, |b| {
                b.on_click(cx.listener(move |_, _, _, cx| {
                    cancel.cancel();
                    tracing::info!("thunderstore install cancel asked");
                    cx.notify();
                }))
            }),
        ))
}

fn placement_text(placement: &Placement) -> String {
    let target = placement
        .target
        .as_ref()
        .map_or_else(|| t!("mods.archive.root").into_owned(), ModPath::to_string);
    match placement.kind {
        PlacementKind::GameRoot => t!("mods.thunderstore.place_root"),
        PlacementKind::BepInEx => t!("mods.thunderstore.place_bepinex", target = target),
        PlacementKind::PluginFolder => t!("mods.thunderstore.place_plugin", target = target),
        PlacementKind::Unrecognised => t!("mods.thunderstore.place_unrecognised", target = target),
        PlacementKind::Plain | _ => t!("mods.thunderstore.place_plain", target = target),
    }
    .into_owned()
}

fn ready_panel(ready: &Ready, cx: &Context<ModsTab>) -> Div {
    let new = ready
        .items
        .iter()
        .filter(|item| item.preview.duplicate_of().is_none())
        .count();
    let items = ready.items.iter().map(|item| {
        let (shown, count) = item.preview.overrides(item.placement.target.as_ref());
        let label = if item.is_dependency {
            t!(
                "mods.thunderstore.item_dependency",
                name = item.package.display_name(),
                version = item.version
            )
        } else {
            t!(
                "mods.thunderstore.item",
                name = item.package.display_name(),
                version = item.version
            )
        };
        div()
            .flex()
            .flex_col()
            .gap_1()
            .pt_2()
            .child(div().child(label))
            .child(theme::muted(placement_text(&item.placement)).text_size(px(12.5)))
            .child(
                theme::muted(t!(
                    "mods.archive.summary",
                    files = item.preview.files(),
                    size = crate::format::bytes(item.preview.size())
                ))
                .text_size(px(12.5)),
            )
            .children((count > 0).then(|| {
                theme::muted(format!(
                    "{} {}",
                    t!("mods.archive.overrides", count = count),
                    shown
                        .iter()
                        .take(SHOWN_OVERRIDES)
                        .map(ModPath::as_str)
                        .collect::<Vec<_>>()
                        .join(", ")
                ))
                .text_size(px(12.5))
            }))
            .children(
                item.preview
                    .has_installer()
                    .then(|| theme::muted(t!("mods.archive.installer")).text_size(px(12.5))),
            )
            .children(
                item.preview
                    .duplicate_of()
                    .map(|(_, name)| theme::muted(t!("mods.thunderstore.duplicate", name = name)).text_size(px(12.5))),
            )
    });
    let skipped = ready.skipped.iter().map(|skipped| {
        theme::muted(t!(
            "mods.thunderstore.skipped",
            package = skipped.package,
            installed = skipped.installed,
            wanted = skipped.wanted
        ))
        .text_size(px(12.5))
    });
    let can_add = new > 0;
    div()
        .flex()
        .flex_col()
        .gap_1()
        .child(div().child(t!("mods.thunderstore.ready", count = new)))
        .children(items)
        .children(skipped)
        .child(
            div()
                .flex()
                .gap(px(10.))
                .pt_2()
                .child(
                    button("ts-add", ButtonKind::Primary, t!("mods.archive.add"), !can_add).when(can_add, |b| {
                        b.on_click(cx.listener(|this, _, _, cx| this.add_packages(cx)))
                    }),
                )
                .child(
                    button("ts-dismiss", ButtonKind::Secondary, t!("common.cancel"), false).on_click(cx.listener(
                        |this, _, _, cx| {
                            if let Some(install) = this.thunderstore.as_mut().and_then(|browser| browser.install.take())
                            {
                                install.discard();
                            }
                            cx.notify();
                        },
                    )),
                ),
        )
}
