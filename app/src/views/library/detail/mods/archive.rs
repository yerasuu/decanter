//! Adding a zip or 7z archive as a mod: it is read and checked first (hashed, every entry looked
//! at) with a progress line and a cancel, then previewed with where its files go and which of the
//! game's they replace, and only extracted once the user says *Add* (design, section 5).

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use decanter_mods::{ArchivePreview, ModPath, Progress};
use gpui::{Context, Div, SharedString, div, prelude::*, px};
use rust_i18n::t;

use super::ModsTab;
use crate::i18n::Text;
use crate::theme;
use crate::views::library::detail::mods::field_row;
use crate::views::library::detail::mods::toggles::{install_choice, install_label};
use crate::widgets::{ButtonKind, Select, button, progress_bar};

/// How often the progress line is redrawn while a blocking read or extraction runs.
const TICK: Duration = Duration::from_millis(200);
/// How many of the game's files a preview names before summing up the rest.
const SHOWN_OVERRIDES: usize = 10;

pub(super) enum Importing {
    Reading(Arc<Progress>),
    Ready {
        preview: Arc<ArchivePreview>,
        target: Option<ModPath>,
        /// Whether the name and target controls are shown; open from the start when the archive
        /// does not say where it goes.
        options: bool,
    },
    Extracting(Arc<Progress>),
}

impl Importing {
    const fn is_working(&self) -> bool {
        matches!(self, Self::Reading(_) | Self::Extracting(_))
    }

    /// Stops a read or an extraction; a previewed archive has nothing running.
    pub(super) fn cancel(&self) {
        if let Self::Reading(progress) | Self::Extracting(progress) = self {
            progress.cancel();
            tracing::info!("mod archive import cancel asked");
        }
    }
}

/// How an archive's reading or extraction ended, for the line the tab shows.
fn outcome_text(err: &decanter_mods::Error) -> Text {
    if matches!(err, decanter_mods::Error::Cancelled) {
        Text::new("mods.archive.cancelled")
    } else {
        Text::new("mods.archive.refused").arg("reason", err)
    }
}

impl ModsTab {
    pub(super) fn pick_archive(cx: &Context<Self>) {
        cx.spawn(async move |this, cx| {
            let Some(file) = rfd::AsyncFileDialog::new()
                .set_title(t!("mods.archive.pick_title"))
                .add_filter(t!("mods.archive.filter"), &["zip", "ZIP", "7z", "7Z"])
                .pick_file()
                .await
            else {
                return;
            };
            let source = file.path().to_path_buf();
            let _ = this.update(cx, |this, cx| this.read_archive(source, cx));
        })
        .detach();
    }

    pub(super) fn read_archive(&mut self, source: PathBuf, cx: &mut Context<Self>) {
        let Some(mods) = self.mods.clone() else {
            return;
        };
        tracing::info!(path = %source.display(), "mod archive picked");
        let progress = Arc::new(Progress::default());
        self.importing = Some(Importing::Reading(Arc::clone(&progress)));
        self.error = None;
        self.notice = None;
        Self::follow(cx);
        cx.spawn(async move |this, cx| {
            let read = tokio::task::spawn_blocking(move || mods.preview_archive(&source, &progress)).await;
            let _ = this.update(cx, |this, cx| {
                this.importing = None;
                match read {
                    Ok(Ok(preview)) => {
                        let name = preview.name().to_owned();
                        this.name_input.update(cx, |input, cx| input.set_text(name, cx));
                        this.importing = Some(Importing::Ready {
                            target: preview.target().cloned(),
                            options: !preview.target_certain(),
                            preview: Arc::new(preview),
                        });
                    }
                    Ok(Err(err)) => {
                        tracing::warn!(error = %err, "mod archive not added");
                        this.show_outcome(outcome_text(&err));
                    }
                    Err(err) => {
                        tracing::error!(error = %err, "reading the mod archive stopped");
                        this.error = Some(Text::verbatim(err));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn add_archive(&mut self, cx: &Context<Self>) {
        let Some(Importing::Ready { preview, target, .. }) = self.importing.take() else {
            return;
        };
        let Some(mods) = self.mods.clone() else {
            return;
        };
        let name = self.name_input.read(cx).text().trim().to_owned();
        let account_id = self.target.account_id.clone();
        let enabled = self.enables_on_install();
        let progress = Arc::new(Progress::default());
        self.importing = Some(Importing::Extracting(Arc::clone(&progress)));
        Self::follow(cx);
        cx.spawn(async move |this, cx| {
            let added = tokio::task::spawn_blocking(move || {
                mods.import_archive(&preview, &name, target, &account_id, enabled, &progress)
            })
            .await;
            let _ = this.update(cx, |this, cx| {
                this.importing = None;
                match added {
                    Ok(Ok(_)) => {}
                    Ok(Err(err)) => {
                        tracing::warn!(error = %err, "mod archive not added");
                        this.show_outcome(outcome_text(&err));
                    }
                    Err(err) => {
                        tracing::error!(error = %err, "extracting the mod archive stopped");
                        this.error = Some(Text::verbatim(err));
                    }
                }
                this.reload(cx);
            });
        })
        .detach();
    }

    /// A cancel shows as a plain line, a refusal as an error.
    fn show_outcome(&mut self, text: Text) {
        if text.is_or_wraps("mods.archive.cancelled") {
            self.notice = Some(text);
        } else {
            self.error = Some(text);
        }
    }

    /// Redraws the progress line until the read or extraction ends.
    fn follow(cx: &Context<Self>) {
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(TICK).await;
                let working = this.update(cx, |this, cx| {
                    cx.notify();
                    this.importing.as_ref().is_some_and(Importing::is_working)
                });
                if !matches!(working, Ok(true)) {
                    break;
                }
            }
        })
        .detach();
    }

    pub(super) fn archive_panel(&self, cx: &Context<Self>) -> Option<Div> {
        match self.importing.as_ref()? {
            Importing::Reading(progress) => Some(progress_panel(
                progress,
                |done, total| t!("mods.archive.reading", done = done, total = total),
                cx,
            )),
            Importing::Extracting(progress) => Some(progress_panel(
                progress,
                |done, total| t!("mods.archive.extracting", done = done, total = total),
                cx,
            )),
            Importing::Ready {
                preview,
                target,
                options,
            } => Some(self.preview_panel(preview, target.as_ref(), *options, cx)),
        }
    }

    fn preview_panel(
        &self,
        preview: &ArchivePreview,
        target: Option<&ModPath>,
        options_open: bool,
        cx: &Context<Self>,
    ) -> Div {
        let entity = cx.entity();
        let folders = std::iter::once((
            SharedString::default(),
            SharedString::from(t!("mods.archive.root").into_owned()),
        ))
        .chain(preview.folders().iter().map(|folder| {
            let folder = SharedString::from(folder.to_string());
            (folder.clone(), folder)
        }));
        let selected = target.map(ModPath::to_string).unwrap_or_default();
        let place = Select::new("mod-archive-target", folders, selected).on_change(move |chosen, _, cx| {
            let chosen = (!chosen.is_empty()).then(|| ModPath::parse(chosen).ok()).flatten();
            entity.update(cx, |this, cx| {
                if let Some(Importing::Ready { target, .. }) = this.importing.as_mut() {
                    *target = chosen;
                }
                cx.notify();
            });
        });
        let (shown, count) = preview.overrides(target);
        let overrides = (count > 0).then(|| {
            let rest = count.saturating_sub(shown.len().min(SHOWN_OVERRIDES));
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
                .children((rest > 0).then(|| theme::muted(t!("library.detail.mods.more", count = rest)).pl_2()))
        });
        let duplicate = preview.duplicate_of();
        let can_add = duplicate.is_none() && !self.name_input.read(cx).text().trim().is_empty();
        theme::card()
            .gap_2()
            .px(px(18.))
            .py(px(14.))
            .child(div().child(t!(
                "mods.archive.compact",
                file = preview.source_file_name(),
                files = preview.files(),
                size = crate::format::bytes(preview.size())
            )))
            .children(overrides)
            .children(
                preview
                    .has_installer()
                    .then(|| theme::muted(t!("mods.archive.installer"))),
            )
            .children(duplicate.map(|(_, name)| theme::error(t!("mods.archive.duplicate", name = name))))
            .child(
                div().flex().child(
                    div()
                        .id("mod-archive-options")
                        .cursor_pointer()
                        .on_click(cx.listener(|this, _, _, cx| {
                            if let Some(Importing::Ready { options, .. }) = this.importing.as_mut() {
                                *options = !*options;
                            }
                            cx.notify();
                        }))
                        .child(theme::muted(format!(
                            "{} {}",
                            if options_open { "\u{25be}" } else { "\u{25b8}" },
                            t!("mods.archive.options")
                        ))),
                ),
            )
            .when(options_open, |panel| {
                panel
                    .children((!preview.target_certain()).then(|| theme::muted(t!("mods.archive.options_reason"))))
                    .child(
                        field_row()
                            .items_center()
                            .child(div().w(px(90.)).child(theme::muted(t!("mods.archive.name"))))
                            .child(div().flex_1().min_w_0().child(self.name_input.clone())),
                    )
                    .child(
                        field_row()
                            .items_center()
                            .child(div().w(px(90.)).child(theme::muted(t!("mods.archive.target"))))
                            .child(place),
                    )
                    .children(
                        preview
                            .stripped()
                            .map(|folder| theme::muted(t!("mods.archive.stripped", folder = folder.as_str()))),
                    )
            })
            .child(install_choice(
                "mod-archive-enable-on-install",
                self.enables_on_install(),
                install_label(),
                ModsTab::set_enable_on_install,
                cx,
            ))
            .child(
                div()
                    .flex()
                    .gap(px(10.))
                    .child(
                        button("mod-archive-add", ButtonKind::Primary, t!("mods.archive.add"), !can_add)
                            .when(can_add, |b| {
                                b.on_click(cx.listener(|this, _, _, cx| this.add_archive(cx)))
                            }),
                    )
                    .child(
                        button("mod-archive-cancel", ButtonKind::Secondary, t!("common.cancel"), false).on_click(
                            cx.listener(|this, _, _, cx| {
                                this.importing = None;
                                tracing::info!("mod archive preview dismissed, nothing added");
                                cx.notify();
                            }),
                        ),
                    ),
            )
    }
}

fn progress_panel(
    progress: &Arc<Progress>,
    line: impl Fn(String, String) -> std::borrow::Cow<'static, str>,
    cx: &Context<ModsTab>,
) -> Div {
    let snapshot = progress.snapshot();
    let stopping = progress.is_cancelled();
    let text = if stopping {
        t!("mods.archive.cancelling")
    } else {
        let (done, total) = snapshot.unwrap_or_default();
        line(crate::format::bytes(done), crate::format::bytes(total))
    };
    let cancel = Arc::clone(progress);
    div()
        .flex()
        .flex_col()
        .gap(px(6.))
        .child(theme::muted(text))
        .child(progress_bar(
            "mod-archive-progress",
            snapshot.map(|(done, total)| done as f64 / total as f64),
            6.,
        ))
        .child(div().flex().child(
            button("mod-archive-stop", ButtonKind::Secondary, t!("common.cancel"), stopping).when(!stopping, |b| {
                b.on_click(cx.listener(move |_, _, _, cx| {
                    cancel.cancel();
                    tracing::info!("mod archive import cancel asked");
                    cx.notify();
                }))
            }),
        ))
}
