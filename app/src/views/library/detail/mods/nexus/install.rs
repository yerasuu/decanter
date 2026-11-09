//! Downloading one file for a Premium account: a link asked for it, the download into the game's
//! staging folder with a progress line and a cancel, checked against the listed size and MD5, the
//! archive read by the mods import with where its files go, and only after *Añadir* the import.

use core::sync::atomic::{AtomicU8, Ordering};
use std::sync::Arc;
use std::time::Duration;

use decanter_mods::{ArchivePreview, GameMods, ModPath, Progress};
use decanter_nexus::{Client, FileId, GameDomain, NexusFile, NexusModId, Staging, Transfer};
use gpui::{Context, Div, SharedString, div, prelude::*, px};
use rust_i18n::t;

use super::ModsTab;
use crate::i18n::Text;
use crate::nexus::failure_text;
use crate::theme;
use crate::views::library::detail::mods::field_row;
use crate::widgets::{ButtonKind, Select, button, progress_bar};

const TICK: Duration = Duration::from_millis(200);
const SHOWN_OVERRIDES: usize = 10;

const LINKING: u8 = 0;
const DOWNLOADING: u8 = 1;
const READING: u8 = 2;
const IMPORTING: u8 = 3;

/// What runs off the UI, shared with the panel that draws it and cancels it.
#[derive(Default)]
pub(in crate::views::library::detail::mods) struct Work {
    phase: AtomicU8,
    transfer: Transfer,
    progress: Progress,
}

impl Work {
    fn cancel(&self) {
        self.transfer.cancel();
        self.progress.cancel();
    }
}

pub(in crate::views::library::detail::mods) enum Install {
    Working(Arc<Work>),
    Ready(Ready),
}

impl Install {
    /// Stops what runs, or drops what was downloaded, off the UI thread since it deletes files.
    pub(super) fn discard(self) {
        match self {
            Self::Working(work) => {
                work.cancel();
                tracing::info!("nexus download cancel asked");
            }
            Self::Ready(ready) => {
                tracing::info!("nexus download dismissed, nothing added");
                tokio::task::spawn_blocking(move || drop(ready));
            }
        }
    }
}

pub(in crate::views::library::detail::mods) struct Ready {
    preview: ArchivePreview,
    name: String,
    target: Option<ModPath>,
    is_md5_checked: bool,
    /// Holds the download until the import is done; dropping it deletes it.
    staging: Staging,
}

struct Request {
    client: Client,
    mods: GameMods,
    domain: GameDomain,
    mod_id: NexusModId,
    file: NexusFile,
    name: String,
}

impl ModsTab {
    pub(super) fn download_file(&mut self, mod_id: NexusModId, file_id: FileId, cx: &mut Context<Self>) {
        let Some(mods) = self.mods.clone() else {
            return;
        };
        let Some(browser) = self.nexus.as_mut() else {
            return;
        };
        let (Some(client), Some(domain)) = (browser.client.clone(), browser.domain().cloned()) else {
            return;
        };
        let Some(selected) = browser.selected.as_ref().filter(|selected| selected.info.id == mod_id) else {
            return;
        };
        let Some(file) = selected.files.get(file_id).cloned() else {
            return;
        };
        let request = Request {
            client,
            mods,
            domain,
            mod_id,
            name: selected.info.name.clone(),
            file,
        };
        let work = Arc::new(Work::default());
        browser.install = Some(Install::Working(Arc::clone(&work)));
        browser.failure = None;
        self.error = None;
        self.notice = None;
        tracing::info!(mod_id = %mod_id, file_id = %file_id, "nexus download started");
        Self::follow_nexus(cx);
        cx.notify();
        cx.spawn(async move |this, cx| {
            let shared = Arc::clone(&work);
            let prepared = tokio::spawn(async move { prepare(request, shared).await }).await;
            let _ = this.update(cx, |this, cx| {
                let Some(browser) = this.nexus.as_mut() else {
                    return;
                };
                browser.install = None;
                match prepared {
                    Ok(Ok(ready)) => browser.install = Some(Install::Ready(ready)),
                    Ok(Err(err)) => {
                        tracing::warn!(%err, retryable = err.is_retryable(), "nexus download not prepared");
                        let text = failure_text(&err);
                        if text.is_or_wraps("mods.nexus.cancelled") {
                            this.notice = Some(text);
                        } else {
                            browser.failure = Some(super::Failure { text, retry: None });
                        }
                    }
                    Err(err) => {
                        tracing::error!(%err, "nexus download task stopped");
                        this.error = Some(Text::verbatim(err));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn add_download(&mut self, cx: &mut Context<Self>) {
        let Some(mods) = self.mods.clone() else {
            return;
        };
        let Some(browser) = self.nexus.as_mut() else {
            return;
        };
        let Some(Install::Ready(ready)) = browser.install.take() else {
            return;
        };
        let account_id = self.target.account_id.clone();
        let work = Arc::new(Work::default());
        work.phase.store(IMPORTING, Ordering::Relaxed);
        browser.install = Some(Install::Working(Arc::clone(&work)));
        Self::follow_nexus(cx);
        cx.notify();
        cx.spawn(async move |this, cx| {
            let added = tokio::task::spawn_blocking(move || {
                let Ready {
                    preview,
                    name,
                    target,
                    staging,
                    ..
                } = ready;
                let added = mods.import_archive(&preview, &name, target, &account_id, &work.progress);
                drop(staging);
                added
            })
            .await;
            let _ = this.update(cx, |this, cx| {
                if let Some(browser) = this.nexus.as_mut() {
                    browser.install = None;
                }
                match added {
                    Ok(Ok(manifest)) => {
                        tracing::info!(mod_id = %manifest.id, "nexus file added as a mod");
                        this.notice = Some(Text::new("mods.nexus.added").arg("name", manifest.name));
                    }
                    Ok(Err(err)) => {
                        tracing::warn!(%err, "nexus file not added");
                        this.error = Some(if matches!(err, decanter_mods::Error::Cancelled) {
                            Text::new("mods.nexus.cancelled")
                        } else {
                            Text::new("mods.nexus.import_failed").arg("reason", err)
                        });
                    }
                    Err(err) => {
                        tracing::error!(%err, "nexus import task stopped");
                        this.error = Some(Text::verbatim(err));
                    }
                }
                this.reload(cx);
            });
        })
        .detach();
    }

    /// Redraws the progress line until the work ends.
    fn follow_nexus(cx: &Context<Self>) {
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(TICK).await;
                let working = this.update(cx, |this, cx| {
                    cx.notify();
                    this.nexus
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

/// The link, the download and the archive's preview, all before anything is added.
async fn prepare(request: Request, work: Arc<Work>) -> decanter_nexus::Result<Ready> {
    let Request {
        client,
        mods,
        domain,
        mod_id,
        file,
        name,
    } = request;
    let joined = |err: tokio::task::JoinError| decanter_nexus::Error::Io {
        path: mods.dir().to_path_buf(),
        source: std::io::Error::other(err),
    };
    let staged_mods = mods.clone();
    let staging = tokio::task::spawn_blocking(move || Staging::new(&staged_mods))
        .await
        .map_err(joined)??;
    let link = decanter_nexus::download_link(&client, &domain, mod_id, file.id).await?;
    work.phase.store(DOWNLOADING, Ordering::Relaxed);
    let downloaded = decanter_nexus::download(&client, &staging, &file, link, &work.transfer).await?;
    work.phase.store(READING, Ordering::Relaxed);
    let reader = Arc::clone(&work);
    let read_mods = mods.clone();
    tokio::task::spawn_blocking(move || -> decanter_nexus::Result<Ready> {
        let preview = read_mods.preview_archive(&downloaded.path, &reader.progress)?;
        let origin = decanter_nexus::origin(&domain, mod_id, &file, &downloaded.host)?;
        tracing::info!(
            mod_id = %mod_id,
            file_id = %file.id,
            files = preview.files(),
            target = ?preview.target().map(ModPath::as_str),
            "nexus file read"
        );
        Ok(Ready {
            target: preview.target().cloned(),
            preview: preview.with_origin(origin),
            name,
            is_md5_checked: downloaded.is_md5_checked,
            staging,
        })
    })
    .await
    .map_err(joined)?
}

fn working_panel(work: &Arc<Work>, cx: &Context<ModsTab>) -> Div {
    let stopping = work.progress.is_cancelled() || work.transfer.is_cancelled();
    let (snapshot, text) = match work.phase.load(Ordering::Relaxed) {
        LINKING => (None, t!("mods.nexus.linking")),
        DOWNLOADING => {
            let snapshot = work.transfer.snapshot();
            let (done, total) = snapshot.unwrap_or_default();
            (
                snapshot,
                t!(
                    "mods.nexus.downloading",
                    done = crate::format::bytes(done),
                    total = crate::format::bytes(total)
                ),
            )
        }
        READING => (work.progress.snapshot(), t!("mods.nexus.reading")),
        _ => (work.progress.snapshot(), t!("mods.nexus.importing")),
    };
    let text = if stopping { t!("mods.archive.cancelling") } else { text };
    let cancel = Arc::clone(work);
    div()
        .flex()
        .flex_col()
        .gap(px(6.))
        .child(theme::muted(text))
        .child(progress_bar(
            "nx-progress",
            snapshot.map(|(done, total)| done as f64 / total as f64),
            6.,
        ))
        .child(div().flex().child(
            button("nx-stop", ButtonKind::Secondary, t!("common.cancel"), stopping).when(!stopping, |b| {
                b.on_click(cx.listener(move |_, _, _, cx| {
                    cancel.cancel();
                    tracing::info!("nexus download cancel asked");
                    cx.notify();
                }))
            }),
        ))
}

fn ready_panel(ready: &Ready, cx: &Context<ModsTab>) -> Div {
    let entity = cx.entity();
    let preview = &ready.preview;
    let options = core::iter::once((
        SharedString::default(),
        SharedString::from(t!("mods.archive.root").into_owned()),
    ))
    .chain(preview.folders().iter().map(|folder| {
        let folder = SharedString::from(folder.to_string());
        (folder.clone(), folder)
    }));
    let selected = ready.target.as_ref().map(ModPath::to_string).unwrap_or_default();
    let place = Select::new("nx-target", options, selected).on_change(move |chosen, _, cx| {
        let chosen = (!chosen.is_empty()).then(|| ModPath::parse(chosen).ok()).flatten();
        entity.update(cx, |this, cx| {
            if let Some(Install::Ready(ready)) = this.nexus.as_mut().and_then(|browser| browser.install.as_mut()) {
                ready.target = chosen;
            }
            cx.notify();
        });
    });
    let (shown, count) = preview.overrides(ready.target.as_ref());
    let overrides = if count == 0 {
        theme::muted(t!("mods.archive.overrides_none"))
    } else {
        div()
            .flex()
            .flex_col()
            .child(theme::muted(t!("mods.archive.overrides", count = count)))
            .children(
                shown
                    .iter()
                    .take(SHOWN_OVERRIDES)
                    .map(|path| theme::mono().pl_2().text_size(px(12.5)).child(path.to_string())),
            )
    };
    let duplicate = preview.duplicate_of();
    let can_add = duplicate.is_none();
    let checked = if ready.is_md5_checked {
        t!("mods.nexus.md5_checked")
    } else {
        t!("mods.nexus.md5_unchecked")
    };
    div()
        .flex()
        .flex_col()
        .gap_2()
        .child(div().child(t!(
            "mods.nexus.ready",
            name = ready.name,
            file = preview.source_file_name()
        )))
        .child(theme::muted(checked))
        .child(
            field_row()
                .items_center()
                .child(div().w(px(90.)).child(theme::muted(t!("mods.archive.target"))))
                .child(place),
        )
        .child(theme::muted(t!(
            "mods.archive.summary",
            files = preview.files(),
            size = crate::format::bytes(preview.size())
        )))
        .children(
            preview
                .stripped()
                .map(|folder| theme::muted(t!("mods.archive.stripped", folder = folder.as_str()))),
        )
        .child(overrides)
        .children(
            preview
                .has_installer()
                .then(|| theme::muted(t!("mods.archive.installer"))),
        )
        .children(duplicate.map(|(_, name)| theme::error(t!("mods.archive.duplicate", name = name))))
        .child(
            div()
                .flex()
                .gap(px(10.))
                .child(
                    button("nx-add", ButtonKind::Primary, t!("mods.archive.add"), !can_add).when(can_add, |b| {
                        b.on_click(cx.listener(|this, _, _, cx| this.add_download(cx)))
                    }),
                )
                .child(
                    button("nx-dismiss", ButtonKind::Secondary, t!("common.cancel"), false).on_click(cx.listener(
                        |this, _, _, cx| {
                            if let Some(install) = this.nexus.as_mut().and_then(|browser| browser.install.take()) {
                                install.discard();
                            }
                            cx.notify();
                        },
                    )),
                ),
        )
}
