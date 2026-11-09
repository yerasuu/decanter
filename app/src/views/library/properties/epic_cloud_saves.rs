//! An Epic Games title's cloud saves: the per-game mode (off until the user picks one), the save
//! folder the catalog names, a check that only reads, and the restore and upload buttons. A
//! transfer that would replace newer data comes back as a decision and is only carried out once
//! the user confirms it here; see `crate::epic_saves` and `decanter_epic`'s `saves` for the rules.

use std::rc::Rc;

use decanter_core::InstalledGame;
use decanter_epic::{
    Approval, Direction, SaveCheck, SaveError, SaveFolder, SaveOutcome, SaveTarget, SaveVerdict, Snapshot,
};
use gpui::{Context, SharedString, Window, prelude::*};
use rust_i18n::t;

use super::{Target, error_line, field_row, not_installed, pane, row, rows};
use crate::epic_saves::SaveMode;
use crate::i18n::Text;
use crate::theme;
use crate::views::library::fmt;
use crate::widgets::{ButtonKind, button, confirm_panel};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Busy {
    Checking,
    Transferring(Direction),
}

/// A transfer the user has to decide on, with the snapshot they are shown.
struct Decision {
    direction: Direction,
    snapshot: Option<Snapshot>,
}

pub(super) struct EpicCloudSavesTab {
    target: Rc<Target>,
    installed: Option<InstalledGame>,
    mode: SaveMode,
    /// `None` while it is being resolved.
    folder: Option<Result<SaveFolder, Text>>,
    busy: Option<Busy>,
    check: Option<SaveCheck>,
    done: Option<Text>,
    error: Option<Text>,
    decision: Option<Decision>,
}

impl EpicCloudSavesTab {
    pub(super) fn new(target: Rc<Target>, cx: &mut Context<Self>) -> Self {
        let mode = crate::epic_saves::mode(&target.account_id, &target.product_id);
        let mut this = Self {
            target,
            installed: None,
            mode,
            folder: None,
            busy: None,
            check: None,
            done: None,
            error: None,
            decision: None,
        };
        this.reload(cx);
        this
    }

    fn reload(&mut self, cx: &mut Context<Self>) {
        self.installed = crate::core::installed_game(&self.target.account_id, &self.target.product_id);
        let Some(installed) = self.installed.clone() else {
            return;
        };
        let target = Rc::clone(&self.target);
        self.folder = None;
        cx.spawn(async move |this, cx| {
            let folder = crate::epic_saves::folder(&target.account_id, &installed).await;
            let _ = this.update(cx, |this, cx| {
                this.folder = Some(folder);
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn set_mode(&mut self, mode: SaveMode, cx: &mut Context<Self>) {
        match crate::epic_saves::set_mode(&self.target.account_id, &self.target.product_id, mode) {
            Ok(()) => {
                self.mode = mode;
                self.error = None;
            }
            Err(err) => self.error = Some(err),
        }
        self.decision = None;
        cx.notify();
    }

    fn ready_folder(&self) -> Option<SaveFolder> {
        self.folder.as_ref()?.as_ref().ok().cloned()
    }

    fn run_check(&mut self, cx: &mut Context<Self>) {
        let Some(folder) = self.ready_folder() else {
            return;
        };
        let last_sync = self
            .installed
            .as_ref()
            .and_then(|installed| installed.cloud_save_last_sync);
        let target = Rc::clone(&self.target);
        self.start(Busy::Checking, cx);
        cx.spawn(async move |this, cx| {
            let checked = match SaveTarget::new(&target.account_id, &target.product_id, &folder) {
                Ok(save_target) => decanter_epic::check_saves(save_target, last_sync).await,
                Err(err) => Err(err),
            };
            let _ = this.update(cx, |this, cx| {
                match checked {
                    Ok(check) => this.check = Some(check),
                    Err(err) => this.fail(&err),
                }
                this.busy = None;
                cx.notify();
            });
        })
        .detach();
    }

    fn transfer(&mut self, direction: Direction, approval: Approval, cx: &mut Context<Self>) {
        let Some(folder) = self.ready_folder() else {
            return;
        };
        let last_sync = self
            .installed
            .as_ref()
            .and_then(|installed| installed.cloud_save_last_sync);
        let target = Rc::clone(&self.target);
        self.start(Busy::Transferring(direction), cx);
        tracing::info!(
            account_id = target.account_id,
            app_name = target.product_id,
            ?direction,
            ?approval,
            "epic cloud saves transfer asked from the properties"
        );
        cx.spawn(async move |this, cx| {
            let moved = match SaveTarget::new(&target.account_id, &target.product_id, &folder) {
                Ok(save_target) => match direction {
                    Direction::Download => decanter_epic::download_saves(save_target, last_sync, approval).await,
                    _ => decanter_epic::upload_saves(save_target, last_sync, approval).await,
                },
                Err(err) => Err(err),
            };
            let _ = this.update(cx, |this, cx| {
                this.finish(direction, moved, cx);
                this.busy = None;
                cx.notify();
            });
        })
        .detach();
    }

    fn start(&mut self, busy: Busy, cx: &mut Context<Self>) {
        self.busy = Some(busy);
        self.error = None;
        self.done = None;
        self.decision = None;
        cx.notify();
    }

    fn finish(&mut self, direction: Direction, moved: Result<SaveOutcome, SaveError>, cx: &mut Context<Self>) {
        let account_id = self.target.account_id.as_str();
        let app_name = self.target.product_id.as_str();
        match moved {
            Ok(outcome) => {
                crate::epic_saves::record(account_id, app_name, &outcome);
                let size = crate::format::bytes(outcome.bytes);
                self.done = Some(match outcome.moved {
                    Some(Direction::Download) => Text::new("epic_saves.done.downloaded")
                        .arg("count", outcome.files)
                        .arg("size", size),
                    Some(_) => Text::new("epic_saves.done.uploaded")
                        .arg("count", outcome.files)
                        .arg("size", size),
                    None => Text::new("epic_saves.done.nothing"),
                });
                self.check = None;
                self.reload(cx);
            }
            Err(SaveError::NeedsConfirmation { snapshot, .. }) => {
                self.decision = Some(Decision { direction, snapshot });
            }
            Err(err) => self.fail(&err),
        }
    }

    fn fail(&mut self, err: &SaveError) {
        tracing::warn!(
            account_id = self.target.account_id,
            app_name = self.target.product_id,
            error = %err,
            retryable = err.is_retryable(),
            "epic cloud saves request failed"
        );
        self.error = Some(crate::epic_saves::error_text(err));
    }

    fn confirmation(&self, cx: &Context<Self>) -> Option<impl IntoElement> {
        let decision = self.decision.as_ref()?;
        let direction = decision.direction;
        let seen = decision.snapshot.as_ref().map(|snapshot| snapshot.name.clone());
        let (title, body, yes) = match direction {
            Direction::Download => (
                t!("epic_saves.confirm.download_title"),
                t!("epic_saves.confirm.download_body"),
                t!("epic_saves.confirm.download_yes"),
            ),
            _ => (
                t!("epic_saves.confirm.upload_title"),
                t!("epic_saves.confirm.upload_body"),
                t!("epic_saves.confirm.upload_yes"),
            ),
        };
        Some(confirm_panel(
            "epic-saves-confirm",
            title,
            body,
            yes,
            cx.listener(move |this, _, _, cx| {
                tracing::info!(
                    account_id = this.target.account_id,
                    app_name = this.target.product_id,
                    ?direction,
                    "epic cloud saves transfer confirmed by the user"
                );
                this.transfer(direction, Approval::Confirmed { seen: seen.clone() }, cx);
            }),
            cx.listener(|this, _, _, cx| {
                this.decision = None;
                cx.notify();
            }),
        ))
    }

    fn mode_row(&self, cx: &Context<Self>) -> impl IntoElement + use<> {
        let busy = self.busy.is_some();
        let choices = [
            (SaveMode::Off, t!("epic_saves.mode.off")),
            (SaveMode::DownloadOnly, t!("epic_saves.mode.download")),
            (SaveMode::Sync, t!("epic_saves.mode.sync")),
        ];
        field_row().children(choices.into_iter().enumerate().map(|(index, (mode, label))| {
            let kind = if mode == self.mode {
                ButtonKind::Primary
            } else {
                ButtonKind::Secondary
            };
            button(("epic-saves-mode", index), kind, label, busy).when(!busy, |b| {
                b.on_click(cx.listener(move |this, _, _, cx| this.set_mode(mode, cx)))
            })
        }))
    }

    fn actions_row(&self, cx: &Context<Self>) -> impl IntoElement + use<> {
        let disabled = self.busy.is_some() || self.ready_folder().is_none();
        let label = |busy: Busy, idle: &'static str, working: &'static str| {
            if self.busy == Some(busy) { t!(working) } else { t!(idle) }
        };
        let check = label(Busy::Checking, "epic_saves.check", "epic_saves.checking");
        let download = label(
            Busy::Transferring(Direction::Download),
            "epic_saves.download",
            "epic_saves.downloading",
        );
        let upload = label(
            Busy::Transferring(Direction::Upload),
            "epic_saves.upload",
            "epic_saves.uploading",
        );
        let can_upload = self.mode == SaveMode::Sync;
        field_row()
            .child(
                button("epic-saves-check", ButtonKind::Secondary, check, disabled).when(!disabled, |b| {
                    b.on_click(cx.listener(|this, _, _, cx| this.run_check(cx)))
                }),
            )
            .child(
                button("epic-saves-download", ButtonKind::Primary, download, disabled).when(!disabled, |b| {
                    b.on_click(cx.listener(|this, _, _, cx| {
                        this.transfer(Direction::Download, Approval::Automatic, cx);
                    }))
                }),
            )
            .when(can_upload, |row| {
                row.child(
                    button("epic-saves-upload", ButtonKind::Secondary, upload, disabled).when(!disabled, |b| {
                        b.on_click(cx.listener(|this, _, _, cx| {
                            this.transfer(Direction::Upload, Approval::Automatic, cx);
                        }))
                    }),
                )
            })
    }
}

impl Render for EpicCloudSavesTab {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(installed) = &self.installed else {
            return not_installed();
        };
        let folder: SharedString = match &self.folder {
            None => t!("epic_saves.folder_resolving").into_owned().into(),
            Some(Ok(folder)) => folder.path().display().to_string().into(),
            Some(Err(reason)) => reason.into(),
        };
        let last_sync = installed.cloud_save_last_sync.map_or_else(
            || t!("library.properties.saves.never").into_owned(),
            |last| fmt::unix_date(last).unwrap_or_default(),
        );
        let is_on = self.mode != SaveMode::Off;
        let mode_note = match self.mode {
            SaveMode::Off => t!("epic_saves.mode_off_note"),
            SaveMode::DownloadOnly => t!("epic_saves.mode_download_note"),
            SaveMode::Sync => t!("epic_saves.mode_sync_note"),
        };
        pane()
            .child(theme::muted(t!("epic_saves.note")))
            .child(self.mode_row(cx))
            .child(theme::muted(mode_note))
            .child(
                rows()
                    .child(row(t!("epic_saves.folder"), folder))
                    .child(row(t!("library.properties.saves.last_sync"), last_sync)),
            )
            .when(is_on, |pane| pane.child(self.actions_row(cx)))
            .children(self.check.as_ref().filter(|_| is_on).map(check_lines))
            .children(self.confirmation(cx))
            .children(self.done.as_ref().map(theme::muted))
            .children(error_line(self.error.as_ref()))
    }
}

fn check_lines(check: &SaveCheck) -> impl IntoElement {
    let verdict = match check.verdict {
        SaveVerdict::NoSaves => t!("epic_saves.verdict.no_saves"),
        SaveVerdict::RemoteOnly => t!("epic_saves.verdict.remote_only"),
        SaveVerdict::LocalOnly => t!("epic_saves.verdict.local_only"),
        SaveVerdict::SameAge => t!("epic_saves.verdict.same_age"),
        SaveVerdict::RemoteNewer => t!("epic_saves.verdict.remote_newer"),
        SaveVerdict::LocalNewer => t!("epic_saves.verdict.local_newer"),
        _ => t!("epic_saves.verdict.conflict"),
    };
    let date = |seconds: i64| fmt::unix_date(seconds).unwrap_or_default();
    let remote = check.snapshot.as_ref().map_or_else(
        || t!("epic_saves.remote_none").into_owned(),
        |snapshot| t!("epic_saves.remote", count = check.snapshots, date = date(snapshot.time)).into_owned(),
    );
    let local = check.local_newest.map_or_else(
        || t!("epic_saves.local_none").into_owned(),
        |newest| {
            t!(
                "epic_saves.local",
                count = check.local_files,
                size = crate::format::bytes(check.local_bytes),
                date = date(newest)
            )
            .into_owned()
        },
    );
    rows()
        .child(theme::muted(verdict))
        .child(theme::muted(remote))
        .child(theme::muted(local))
}
