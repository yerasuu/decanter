//! Installing one mod.io file: a fresh record and download into the game's staging folder with a
//! progress line and a cancel, the archive read by the mods import, and only after *Añadir* the
//! import itself, with the mod's origin for *Buscar actualizaciones*. Another file of a mod the
//! game has takes that mod's slot.

use core::sync::atomic::{AtomicU8, Ordering};
use std::sync::Arc;
use std::time::Duration;

use decanter_modio::{Client, FileId, Game, Origin, RemoteMod, Staging, Transfer, Virus};
use decanter_mods::{ArchivePreview, GameMods, ModPath, Progress};
use gpui::{Context, Div, div, prelude::*, px};
use rust_i18n::t;

use super::{Failure, ModsTab};
use crate::i18n::Text;
use crate::theme;
use crate::views::library::detail::mods::replace::Replacement;
use crate::views::library::detail::mods::toggles::{install_choice, install_label};
use crate::widgets::{ButtonKind, button, progress_bar};

const TICK: Duration = Duration::from_millis(200);
const SHOWN_OVERRIDES: usize = 5;

const DOWNLOADING: u8 = 0;
const READING: u8 = 1;
const IMPORTING: u8 = 2;

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
                tracing::info!("modio install cancel asked");
            }
            Self::Ready(ready) => {
                tracing::info!(mod_id = %ready.origin.mod_id, "modio install dismissed, nothing added");
                tokio::task::spawn_blocking(move || drop(ready));
            }
        }
    }
}

pub(in crate::views::library::detail::mods) struct Ready {
    name: String,
    version: Option<String>,
    virus: Virus,
    origin: Origin,
    preview: ArchivePreview,
    /// The game's mod from another file of the same mod.io mod, whose slot this one takes.
    replaces: Option<Replacement>,
    keep_previous: bool,
    /// Holds the download until the import is done; dropping it deletes it.
    staging: Staging,
}

struct Request {
    client: Client,
    mods: GameMods,
    game: Game,
    remote: RemoteMod,
    file: FileId,
}

impl ModsTab {
    pub(super) fn modio_install(&mut self, remote: &RemoteMod, file: FileId, cx: &mut Context<Self>) {
        let Some(mods) = self.mods.clone() else {
            return;
        };
        let Some(browser) = self.modio.as_mut() else {
            return;
        };
        let (Some(client), Some(game)) = (browser.client.clone(), browser.game.clone()) else {
            return;
        };
        let work = Arc::new(Work::default());
        browser.install = Some(Install::Working(Arc::clone(&work)));
        browser.failure = None;
        self.error = None;
        self.notice = None;
        tracing::info!(game_id = %game.id, mod_id = %remote.id, file_id = %file, "modio install started");
        Self::modio_follow(cx);
        cx.notify();
        let request = Request {
            client,
            mods,
            game,
            remote: remote.clone(),
            file,
        };
        cx.spawn(async move |this, cx| {
            let shared = Arc::clone(&work);
            let prepared = tokio::spawn(async move { prepare(request, shared).await }).await;
            let _ = this.update(cx, |this, cx| {
                let Some(browser) = this.modio.as_mut() else {
                    return;
                };
                browser.install = None;
                match prepared {
                    Ok(Ok(ready)) => browser.install = Some(Install::Ready(ready)),
                    Ok(Err(decanter_modio::Error::Cancelled)) => {
                        this.notice = Some(Text::new("mods.modio.cancelled"));
                    }
                    Ok(Err(err)) => this.modio_fail(&err, None),
                    Err(err) => {
                        tracing::error!(%err, "modio install task stopped");
                        browser.failure = Some(Failure {
                            text: Text::verbatim(err),
                            retry: None,
                            is_terms: false,
                        });
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn modio_add(&mut self, cx: &mut Context<Self>) {
        let enabled = self.enables_on_install();
        let Some(mods) = self.mods.clone() else {
            return;
        };
        let Some(browser) = self.modio.as_mut() else {
            return;
        };
        let Some(Install::Ready(ready)) = browser.install.take() else {
            return;
        };
        let account_id = self.target.account_id.clone();
        let work = Arc::new(Work::default());
        work.phase.store(IMPORTING, Ordering::Relaxed);
        browser.install = Some(Install::Working(Arc::clone(&work)));
        Self::modio_follow(cx);
        cx.notify();
        cx.spawn(async move |this, cx| {
            let added = tokio::task::spawn_blocking(move || {
                let Ready {
                    name,
                    preview,
                    staging,
                    origin,
                    replaces,
                    keep_previous,
                    ..
                } = ready;
                let target = preview.target().cloned();
                let added = mods
                    .import_archive(&preview, &name, target, &account_id, enabled, &work.progress)
                    .and_then(|manifest| {
                        tracing::info!(mod_id = %origin.mod_id, file_id = %origin.file, added = %manifest.id, "modio mod added");
                        match replaces {
                            Some(replaces) => replaces.apply(&mods, &manifest.id, keep_previous).map(|()| manifest),
                            None => Ok(manifest),
                        }
                    });
                if let Err(err) = &added {
                    tracing::warn!(mod_id = %origin.mod_id, %err, "modio mod not added in full");
                }
                drop(staging);
                added
            })
            .await;
            let _ = this.update(cx, |this, cx| {
                if let Some(browser) = this.modio.as_mut() {
                    browser.install = None;
                }
                match added {
                    Ok(Ok(_)) => this.notice = Some(Text::new("mods.modio.added")),
                    Ok(Err(decanter_mods::Error::Cancelled)) => this.notice = Some(Text::new("mods.modio.cancelled")),
                    Ok(Err(err)) => this.error = Some(Text::new("mods.modio.import_failed").arg("reason", err)),
                    Err(err) => {
                        tracing::error!(%err, "modio import task stopped");
                        this.error = Some(Text::verbatim(err));
                    }
                }
                this.reload(cx);
            });
        })
        .detach();
    }

    /// Redraws the progress line until the work ends.
    fn modio_follow(cx: &Context<Self>) {
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(TICK).await;
                let working = this.update(cx, |this, cx| {
                    cx.notify();
                    this.modio
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

/// The download and the archive's preview, all before anything is added.
async fn prepare(request: Request, work: Arc<Work>) -> decanter_modio::Result<Ready> {
    let Request {
        client,
        mods,
        game,
        remote,
        file,
    } = request;
    let dir = mods.dir().to_path_buf();
    let joined = |err: tokio::task::JoinError| decanter_modio::Error::Io {
        path: dir.clone(),
        source: std::io::Error::other(err),
    };
    let staged = mods.clone();
    let staging = tokio::task::spawn_blocking(move || Staging::new(&staged))
        .await
        .map_err(joined)??;
    work.phase.store(DOWNLOADING, Ordering::Relaxed);
    let downloaded = decanter_modio::download(&client, &staging, &game, remote.id, file, &work.transfer).await?;
    work.phase.store(READING, Ordering::Relaxed);
    let origin = Origin::new(game.id, remote.id, file);
    let version = downloaded.file.version.clone();
    let mod_origin = origin
        .to_mod_origin(version.as_deref(), &downloaded.host)?
        .with_credits(remote.credits());
    let path = downloaded.path;
    let reading = Arc::clone(&work);
    let (preview, replaces) = tokio::task::spawn_blocking(move || {
        let preview = mods.preview_archive(&path, &reading.progress)?;
        let replaces = mods.list()?.iter().rev().find_map(|manifest| {
            let had = Origin::from_mod_origin(manifest.origin.as_ref()?)?;
            (had.game == origin.game && had.mod_id == origin.mod_id && had.file != origin.file)
                .then(|| Replacement::of(manifest))
        });
        Ok::<_, decanter_modio::Error>((preview, replaces))
    })
    .await
    .map_err(joined)??;
    tracing::info!(
        mod_id = %remote.id,
        file_id = %file,
        target = ?preview.target().map(ModPath::as_str),
        files = preview.files(),
        "modio file read"
    );
    Ok(Ready {
        name: remote.name,
        version,
        virus: downloaded.file.virus,
        origin,
        preview: preview.with_origin(mod_origin),
        replaces,
        keep_previous: false,
        staging,
    })
}

pub(super) fn panel(install: &Install, enable_on_install: bool, cx: &Context<ModsTab>) -> Div {
    match install {
        Install::Working(work) => working_panel(work, cx),
        Install::Ready(ready) => ready_panel(ready, enable_on_install, cx),
    }
}

fn working_panel(work: &Arc<Work>, cx: &Context<ModsTab>) -> Div {
    let stopping = work.progress.is_cancelled() || work.transfer.is_cancelled();
    let (snapshot, text) = match work.phase.load(Ordering::Relaxed) {
        DOWNLOADING => {
            let snapshot = work.transfer.snapshot();
            let (done, total) = snapshot.unwrap_or_default();
            (
                snapshot,
                t!(
                    "mods.modio.downloading",
                    done = crate::format::bytes(done),
                    total = crate::format::bytes(total)
                ),
            )
        }
        READING => (work.progress.snapshot(), t!("mods.modio.reading")),
        _ => (work.progress.snapshot(), t!("mods.modio.importing")),
    };
    let text = if stopping { t!("mods.archive.cancelling") } else { text };
    let cancel = Arc::clone(work);
    div()
        .flex()
        .flex_col()
        .gap(px(6.))
        .child(theme::muted(text))
        .child(progress_bar(
            "modio-progress",
            snapshot.map(|(done, total)| done as f64 / total as f64),
            6.,
        ))
        .child(div().flex().child(
            button("modio-stop", ButtonKind::Secondary, t!("common.cancel"), stopping).when(!stopping, |b| {
                b.on_click(cx.listener(move |_, _, _, cx| {
                    cancel.cancel();
                    tracing::info!("modio install cancel asked");
                    cx.notify();
                }))
            }),
        ))
}

fn ready_panel(ready: &Ready, enable_on_install: bool, cx: &Context<ModsTab>) -> Div {
    let preview = &ready.preview;
    let target = preview.target();
    let (shown, count) = preview.overrides(target);
    let duplicate = preview.duplicate_of();
    let can_add = duplicate.is_none();
    let place = target.map_or_else(|| t!("mods.archive.root").into_owned(), ModPath::to_string);
    div()
        .flex()
        .flex_col()
        .gap_1()
        .child(div().child(t!(
            "mods.modio.ready",
            name = ready.name,
            version = ready.version.as_deref().unwrap_or("-")
        )))
        .child(theme::muted(t!("mods.modio.place", target = place)).text_size(px(12.5)))
        .child(
            theme::muted(t!(
                "mods.archive.summary",
                files = preview.files(),
                size = crate::format::bytes(preview.size())
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
            preview
                .has_installer()
                .then(|| theme::muted(t!("mods.archive.installer")).text_size(px(12.5))),
        )
        .children(match ready.virus {
            Virus::Harmful => Some(theme::error(t!("mods.modio.virus_harmful")).text_size(px(12.5))),
            Virus::Unscanned => Some(theme::muted(t!("mods.modio.virus_unscanned")).text_size(px(12.5))),
            Virus::Clean | Virus::Malicious | _ => None,
        })
        .children(duplicate.map(|(_, name)| theme::error(t!("mods.archive.duplicate", name = name))))
        .children(ready.replaces.as_ref().filter(|_| can_add).map(|replaces| {
            replaces.block(
                "modio-keep-previous",
                ready.keep_previous,
                |this, cx| {
                    if let Some(Install::Ready(ready)) =
                        this.modio.as_mut().and_then(|browser| browser.install.as_mut())
                    {
                        ready.keep_previous = !ready.keep_previous;
                    }
                    cx.notify();
                },
                cx,
            )
        }))
        .child(
            install_choice(
                "modio-enable-on-install",
                enable_on_install,
                install_label(),
                ModsTab::set_enable_on_install,
                cx,
            )
            .pt_1(),
        )
        .child(
            div()
                .flex()
                .gap(px(10.))
                .pt_2()
                .child(
                    button("modio-add", ButtonKind::Primary, t!("mods.archive.add"), !can_add).when(can_add, |b| {
                        b.on_click(cx.listener(|this, _, _, cx| this.modio_add(cx)))
                    }),
                )
                .child(
                    button("modio-dismiss", ButtonKind::Secondary, t!("common.cancel"), false).on_click(cx.listener(
                        |this, _, _, cx| {
                            if let Some(install) = this.modio.as_mut().and_then(|browser| browser.install.take()) {
                                install.discard();
                            }
                            cx.notify();
                        },
                    )),
                ),
        )
}
