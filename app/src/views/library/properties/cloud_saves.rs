//! Syncing a local save folder against GOG's Cloud Saves — see
//! `decanter_gog::cloud_saves` for the sync protocol and
//! `decanter_gog::save_location` for how the folder itself gets found:
//! GOG's own remote-config for this game's build, when it reports one,
//! with manual entry (and the folder picker) as the fallback for whatever
//! it doesn't cover.

use std::rc::Rc;

use decanter_core::InstalledGame;
use decanter_gog::cloud_saves::{ForceDirection, SyncAction, SyncOutcome};
use decanter_gog::save_location::SaveLocation;
use gpui::{Context, Entity, SharedString, Window, div, prelude::*};
use rust_i18n::t;

use super::{Target, error_line, field_row, not_installed, pane, row, rows};
use crate::i18n::Text;
use crate::theme;
use crate::views::library::fmt;
use crate::widgets::{ButtonKind, InputEvent, TextInput, button};

pub(super) struct CloudSavesTab {
    target: Rc<Target>,
    installed: Option<InstalledGame>,
    path_input: Entity<TextInput>,
    syncing: bool,
    last_outcome: Option<SyncAction>,
    error: Option<Text>,
    detecting: bool,
    detected: Option<Vec<SaveLocation>>,
    auto_detect_attempted: bool,
}

impl CloudSavesTab {
    pub(super) fn new(target: Rc<Target>, _: &mut Window, cx: &mut Context<Self>) -> Self {
        let path_input = cx.new(|cx| TextInput::new(Text::new("library.properties.saves.folder_placeholder"), cx));
        cx.subscribe(&path_input, |this, _, event, cx| {
            if matches!(event, InputEvent::Changed) {
                this.refill_path(cx);
                cx.notify();
            }
        })
        .detach();
        let mut this = Self {
            target,
            installed: None,
            path_input,
            syncing: false,
            last_outcome: None,
            error: None,
            detecting: false,
            detected: None,
            auto_detect_attempted: false,
        };
        this.reload(cx);
        this
    }

    /// The webview build's `generation` bump: re-reads the manifest, then
    /// re-runs what its render and effect did on every such re-read.
    fn reload(&mut self, cx: &mut Context<Self>) {
        self.installed = crate::core::installed_game(&self.target.account_id, &self.target.product_id);
        self.refill_path(cx);
        self.maybe_auto_detect(cx);
        cx.notify();
    }

    /// An empty field always falls back to the saved folder, so clearing it
    /// shows what's actually in effect rather than a blank that saves nothing.
    fn refill_path(&self, cx: &mut Context<Self>) {
        let Some(saved) = self
            .installed
            .as_ref()
            .and_then(|installed| installed.cloud_save_path.clone())
        else {
            return;
        };
        if self.path_input.read(cx).text().is_empty() {
            self.path_input.update(cx, |input, cx| input.set_text(saved, cx));
        }
    }

    /// Runs once, on first open of a game with no saved path yet — nothing
    /// to override, so a single unambiguous location is saved right away
    /// instead of waiting on a click. Checked on every reload so a manual
    /// "Guardar carpeta" doesn't leave this stale, but
    /// `auto_detect_attempted` keeps it from ever firing a second time.
    fn maybe_auto_detect(&mut self, cx: &mut Context<Self>) {
        if self.auto_detect_attempted {
            return;
        }
        let Some(current) = &self.installed else {
            return;
        };
        if current.cloud_save_path.is_some() {
            return;
        }
        self.auto_detect_attempted = true;
        // Silent: a game with no auto-detectable location is the ordinary
        // case, not something worth an error banner the moment the tab opens.
        self.run_detection(false, cx);
    }

    /// Asks GOG's remote-config for this game's save locations and, when it
    /// names exactly one, saves it right away — no point making the user
    /// retype what was just found. `report_errors` silences the "nothing
    /// found" / request-failed messages for the automatic first-run attempt,
    /// where a miss is the ordinary case rather than something to flag; the
    /// explicit "Detectar automáticamente" button always reports them.
    fn run_detection(&mut self, report_errors: bool, cx: &mut Context<Self>) {
        let target = Rc::clone(&self.target);
        self.detecting = true;
        if report_errors {
            self.error = None;
        }
        tracing::debug!(
            account_id = target.account_id,
            product_id = target.product_id,
            report_errors,
            "detecting gog cloud save location"
        );
        cx.notify();
        cx.spawn(async move |this, cx| {
            let account_id = target.account_id.as_str();
            let game_id = target.product_id.as_str();
            let Some(installed) = crate::core::installed_game(account_id, game_id) else {
                let _ = this.update(cx, |this, cx| {
                    this.detecting = false;
                    cx.notify();
                });
                return;
            };
            let outcome = crate::core::detect_cloud_save_locations(account_id, &installed).await;
            let _ = this.update(cx, |this, cx| this.finish_detection(outcome, report_errors, cx));
        })
        .detach();
    }

    fn finish_detection(
        &mut self,
        outcome: Result<Vec<SaveLocation>, Text>,
        report_errors: bool,
        cx: &mut Context<Self>,
    ) {
        let account_id = self.target.account_id.as_str();
        let game_id = self.target.product_id.as_str();
        match outcome {
            Ok(locations) if locations.is_empty() => {
                tracing::debug!(account_id, product_id = game_id, "gog reports no cloud save location");
                if report_errors {
                    self.error = Some(Text::new("library.properties.saves.none_reported"));
                }
                self.detected = None;
            }
            Ok(locations) => {
                if let [only] = locations.as_slice() {
                    self.save_detected(only, report_errors, cx);
                }
                self.detected = Some(locations);
            }
            Err(err) if report_errors => {
                tracing::error!(account_id, product_id = game_id, error = %err, "cloud save location detection failed");
                self.error = Some(err);
            }
            Err(err) => {
                tracing::debug!(account_id, product_id = game_id, error = %err, "cloud save location detection failed, not reported to user");
            }
        }
        self.detecting = false;
        cx.notify();
    }

    fn save_detected(&mut self, only: &SaveLocation, report_errors: bool, cx: &mut Context<Self>) {
        let target = Rc::clone(&self.target);
        let account_id = target.account_id.as_str();
        let game_id = target.product_id.as_str();
        let resolved = only.path.to_string_lossy().into_owned();
        self.path_input
            .update(cx, |input, cx| input.set_text(resolved.clone(), cx));
        match crate::core::set_cloud_save_path(account_id, game_id, Some(&resolved)) {
            Ok(()) => {
                tracing::info!(
                    account_id,
                    product_id = game_id,
                    path = resolved,
                    "cloud save path auto-detected and saved"
                );
                self.reload(cx);
            }
            Err(err) if report_errors => {
                tracing::error!(account_id, product_id = game_id, error = %err, "failed to save auto-detected cloud save path");
                self.error = Some(err.localized());
            }
            Err(err) => {
                tracing::warn!(account_id, product_id = game_id, error = %err, "failed to save auto-detected cloud save path, not reported to user");
            }
        }
    }

    fn pick_folder(&self, cx: &Context<Self>) {
        // Saves under Proton live in the game's prefix; natively the dialog
        // opens wherever the system's last pick was.
        let starting = crate::launch::in_prefixes()
            .then(|| crate::core::prefix_for(&self.target.account_id, &self.target.product_id))
            .flatten()
            .as_ref()
            .map(decanter_runtime::Prefix::drive_c);
        cx.spawn(async move |this, cx| {
            let mut dialog = rfd::AsyncFileDialog::new().set_title(t!("library.properties.saves.pick_title"));
            if let Some(starting) = starting {
                dialog = dialog.set_directory(starting);
            }
            let Some(folder) = dialog.pick_folder().await else {
                return;
            };
            let path = folder.path().to_string_lossy().into_owned();
            tracing::debug!(path, "cloud save folder picked from file dialog");
            let _ = this.update(cx, |this, cx| {
                this.path_input.update(cx, |input, cx| input.set_text(path, cx));
                cx.notify();
            });
        })
        .detach();
    }

    fn save_path(&mut self, cx: &mut Context<Self>) {
        let value = self.path_input.read(cx).text().trim().to_string();
        let account_id = self.target.account_id.as_str();
        let product_id = self.target.product_id.as_str();
        match crate::core::set_cloud_save_path(account_id, product_id, Some(&value)) {
            Ok(()) => {
                tracing::info!(account_id, product_id, path = value, "cloud save path set");
                self.error = None;
                self.reload(cx);
            }
            Err(err) => {
                tracing::error!(account_id, product_id, path = value, error = %err, "failed to save cloud save path");
                self.error = Some(err.localized());
                cx.notify();
            }
        }
    }

    fn sync(&mut self, force: Option<ForceDirection>, cx: &mut Context<Self>) {
        let Some(installed) = &self.installed else {
            return;
        };
        let Some(local_root) = installed.cloud_save_path.clone() else {
            return;
        };
        let last_sync = installed.cloud_save_last_sync;
        let target = Rc::clone(&self.target);
        self.syncing = true;
        self.error = None;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let outcome = run_cloud_sync(
                target.account_id.clone(),
                target.product_id.clone(),
                local_root,
                last_sync,
                force,
            )
            .await;
            let _ = this.update(cx, |this, cx| {
                match outcome {
                    Ok(outcome) => {
                        this.last_outcome = Some(outcome.action);
                        this.reload(cx);
                    }
                    Err(err) => this.error = Some(err),
                }
                this.syncing = false;
                cx.notify();
            });
        })
        .detach();
    }
}

impl Render for CloudSavesTab {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(installed) = &self.installed else {
            return not_installed();
        };

        let detect_label = if self.detecting {
            t!("library.properties.saves.detecting")
        } else {
            t!("library.properties.saves.detect")
        };
        let detect_row = field_row().child(
            button("detect-saves", ButtonKind::Secondary, detect_label, self.detecting).when(!self.detecting, |b| {
                // A manual click always wants the folder saved too — the
                // same one-click "just find it" the automatic first-run
                // attempt does.
                b.on_click(cx.listener(|this, _, _, cx| this.run_detection(true, cx)))
            }),
        );

        let choices = self
            .detected
            .as_ref()
            .filter(|locations| locations.len() > 1)
            .map(|locations| {
                field_row().children(locations.iter().enumerate().map(|(index, location)| {
                    let path: SharedString = location.path.to_string_lossy().into_owned().into();
                    button(
                        ("save-location", index),
                        ButtonKind::Secondary,
                        location.name.clone(),
                        false,
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.path_input.update(cx, |input, cx| input.set_text(path.clone(), cx));
                        cx.notify();
                    }))
                }))
            });

        let last_sync = installed.cloud_save_last_sync.map_or_else(
            || t!("library.properties.saves.never").into_owned(),
            |last| fmt::unix_date(last).unwrap_or_default(),
        );

        let path_empty = self.path_input.read(cx).text().trim().is_empty();
        let (pick_label, save_label) = (t!("library.properties.saves.pick"), t!("library.properties.saves.save"));
        let path_row = field_row()
            .child(div().flex_1().min_w_0().child(self.path_input.clone()))
            .child(
                button("pick-saves", ButtonKind::Secondary, pick_label, false)
                    .on_click(cx.listener(|this, _, _, cx| this.pick_folder(cx))),
            )
            .child(
                button("save-saves-path", ButtonKind::Primary, save_label, path_empty).when(!path_empty, |b| {
                    b.on_click(cx.listener(|this, _, _, cx| this.save_path(cx)))
                }),
            );

        let sync_disabled = self.syncing || installed.cloud_save_path.is_none();
        let sync_label = if self.syncing {
            t!("library.properties.saves.syncing")
        } else {
            t!("library.properties.saves.sync")
        };
        let sync_row = field_row()
            .child(
                button("sync-saves", ButtonKind::Primary, sync_label, sync_disabled).when(!sync_disabled, |b| {
                    b.on_click(cx.listener(|this, _, _, cx| this.sync(None, cx)))
                }),
            )
            .children(self.last_outcome.map(|outcome| match outcome {
                SyncAction::Conflict => theme::error(t!("library.properties.saves.conflict")),
                other @ (SyncAction::Upload | SyncAction::Download | SyncAction::None) => {
                    theme::muted(sync_action_label(other))
                }
            }));

        let conflict_row = (self.last_outcome == Some(SyncAction::Conflict)).then(|| {
            let syncing = self.syncing;
            let upload = t!("library.properties.saves.force_upload");
            let download = t!("library.properties.saves.force_download");
            field_row()
                .child(
                    button("force-upload", ButtonKind::Secondary, upload, syncing).when(!syncing, |b| {
                        b.on_click(cx.listener(|this, _, _, cx| this.sync(Some(ForceDirection::Upload), cx)))
                    }),
                )
                .child(
                    button("force-download", ButtonKind::Secondary, download, syncing).when(!syncing, |b| {
                        b.on_click(cx.listener(|this, _, _, cx| this.sync(Some(ForceDirection::Download), cx)))
                    }),
                )
        });

        pane()
            .child(theme::muted(t!("library.properties.saves.note")))
            .child(detect_row)
            .children(choices)
            .child(rows().child(row(t!("library.properties.saves.last_sync"), last_sync)))
            .child(path_row)
            .child(sync_row)
            .children(conflict_row)
            .children(error_line(self.error.as_ref()))
    }
}

fn sync_action_label(action: SyncAction) -> std::borrow::Cow<'static, str> {
    match action {
        SyncAction::Upload => t!("library.properties.saves.uploaded"),
        SyncAction::Download => t!("library.properties.saves.downloaded"),
        SyncAction::None => t!("library.properties.saves.already_synced"),
        SyncAction::Conflict => t!("library.properties.saves.unresolved"),
    }
}

/// Runs one sync cycle and, unless it ended in a conflict, records the new
/// sync timestamp on the manifest — a conflict leaves both sides exactly
/// as they were, so there's nothing to advance yet.
async fn run_cloud_sync(
    account_id: String,
    product_id: String,
    local_root: String,
    last_sync: Option<i64>,
    force: Option<ForceDirection>,
) -> Result<SyncOutcome, Text> {
    tracing::info!(account_id, product_id, local_root, force = ?force, "cloud save sync started");
    let outcome = crate::achv_try!(
        decanter_gog::cloud_saves::sync(
            &account_id,
            &product_id,
            std::path::Path::new(&local_root),
            last_sync,
            force
        )
        .await
        .map_err(Text::verbatim)
        // Bubbles up to the properties tab's red error text.
        .inspect_err(|err| tracing::error!(account_id, product_id, error = %err, "cloud save sync failed"))
    );
    tracing::info!(account_id, product_id, action = ?outcome.action, "cloud save sync finished");
    if outcome.action != SyncAction::Conflict {
        crate::achv_try!(
            crate::core::record_cloud_save_sync(&account_id, &product_id, outcome.last_sync).inspect_err(
                |err| tracing::warn!(account_id, product_id, error = %err, "failed to record cloud save sync timestamp")
            )
        );
    }
    Ok(outcome)
}
