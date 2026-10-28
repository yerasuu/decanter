use std::borrow::Cow;

use gpui::{AnyElement, Context, Div, FontWeight, SharedString, div, prelude::*, px};
use rust_i18n::t;

use gpui::Task;

use super::installer::Installer;
use super::{FormEvent, GameForm, Rows};
use crate::external::import::Mode as ImportMode;
use crate::i18n::Text;
use crate::theme;
use crate::widgets::{ButtonKind, button};

/// What the form keeps for a game run under Proton: the prefix it can be
/// installed into or imported from, and its UMU entry.
pub(super) struct Wine {
    /// An edit keeps its Proton pin for the installer too.
    pub(super) proton: Option<String>,
    /// The library folders a new game can go in, with their free space.
    folders: Vec<(std::path::PathBuf, Option<u64>)>,
    /// Another tool's prefix to move in on save, picked by folder.
    import_from: Option<std::path::PathBuf>,
    import_mode: ImportMode,
    /// Set while a save is bringing a prefix in.
    importing: bool,
    progress: std::sync::Arc<crate::external::import::Progress>,
    /// Redraws the progress bar while `importing`: the copy runs on a
    /// blocking thread that can't notify this view itself.
    _ticker: Option<Task<()>>,
    /// The UMU database rows the name matched, once looked up.
    pub(super) umu_matches: Option<Result<Vec<crate::external::umu::Match>, Text>>,
    pub(super) umu_searching: bool,
    pub(super) _umu_work: Option<Task<()>>,
}

impl Wine {
    pub(super) fn new(profile_id: &str, proton: Option<String>) -> Self {
        Self {
            proton,
            folders: library_folders(profile_id),
            import_from: None,
            import_mode: ImportMode::Copy,
            importing: false,
            progress: Default::default(),
            _ticker: None,
            umu_matches: None,
            umu_searching: false,
            _umu_work: None,
        }
    }
}

impl Drop for GameForm {
    /// Leaving the form ends any chance of retrying a failed copy into its
    /// staging directory, so that goes. One still running is left to
    /// finish: it adds the game on its own.
    fn drop(&mut self) {
        if !self.wine.importing && self.wine.import_from.is_some() {
            crate::external::import::abandon(&self.profile_id, &self.id);
        }
    }
}

impl GameForm {
    /// Where the executable picker opens: the imported prefix's `C:`, or
    /// this game's after an installer ran there.
    pub(super) fn prefix_executable_start(&self) -> Option<std::path::PathBuf> {
        match (&self.wine.import_from, &self.installer) {
            (Some(picked), _) => crate::external::import::wine_root(picked).map(|root| root.join("drive_c")),
            (None, Installer::Done) => {
                crate::external::drive_c(&self.profile_id, &self.id, self.library_root.as_deref())
            }
            (None, _) => None,
        }
    }

    /// Starts bringing the picked prefix in, which saves `draft` once it
    /// lands; `draft` back when there is no prefix to import.
    pub(super) fn import_on_save(
        &mut self,
        draft: crate::external::Draft,
        cx: &Context<Self>,
    ) -> Option<crate::external::Draft> {
        let Some(picked) = self.wine.import_from.clone() else {
            return Some(draft);
        };
        self.wine.importing = true;
        self.wine.progress = std::sync::Arc::default();
        let (profile_id, id, mode) = (self.profile_id.clone(), self.id.clone(), self.wine.import_mode);
        let progress = std::sync::Arc::clone(&self.wine.progress);
        self.wine._ticker = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(std::time::Duration::from_millis(250))
                    .await;
                let still = this.update(cx, |this, cx| {
                    cx.notify();
                    this.wine.importing
                });
                if !matches!(still, Ok(true)) {
                    return;
                }
            }
        }));
        self._work = Some(cx.spawn(async move |this, cx| {
            let result = tokio::task::spawn_blocking(move || {
                crate::external::import::import(&profile_id, &id, &picked, mode, &progress, draft)
            })
            .await
            .unwrap_or_else(|err| Err(Text::verbatim(err)));
            let _ = this.update(cx, |this, cx| {
                this.wine.importing = false;
                match result {
                    Ok(()) => cx.emit(FormEvent::Saved),
                    Err(err) => this.error = Some(err),
                }
                cx.notify();
            });
        }));
        None
    }

    pub(super) const fn busy(&self) -> bool {
        matches!(self.installer, Installer::Running) || self.wine.importing
    }

    pub(super) fn save_label(&self) -> Cow<'static, str> {
        match (
            self.editing,
            self.wine.importing,
            self.wine.import_from.is_some(),
            self.wine.import_mode,
        ) {
            (true, ..) => t!("external.form.save.edit"),
            (false, true, _, ImportMode::Copy) => t!("external.form.save.copying"),
            (false, true, _, ImportMode::Move) => t!("external.form.save.moving"),
            (false, false, true, ImportMode::Copy) => t!("external.form.save.copy"),
            (false, false, true, ImportMode::Move) => t!("external.form.save.move"),
            (false, false, false, _) => t!("external.form.save.add"),
        }
    }

    pub(super) fn wine_rows(&self, cx: &Context<Self>) -> Rows {
        Rows {
            folder: self.folder_row(cx),
            installer: (!self.editing && self.prefix_of.is_none()).then(|| self.prefix_installer_row(cx)),
            umu: Some(self.umu_section(cx)),
            progress: self.wine.importing.then(|| self.import_progress()),
        }
    }

    /// Picks another tool's prefix to bring in, then the game inside it.
    /// Nothing moves until the game is saved.
    fn pick_prefix(&mut self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            let Some(folder) = rfd::AsyncFileDialog::new()
                .set_title(t!("external.form.import.pick_title"))
                .pick_folder()
                .await
            else {
                return;
            };
            let picked = folder.path().to_path_buf();
            let _ = this.update(cx, |this, cx| {
                if crate::external::import::wine_root(&picked).is_none() {
                    this.error = Some(Text::new("external.errors.not_a_prefix"));
                    cx.notify();
                    return;
                }
                tracing::debug!(prefix = %picked.display(), "prefix picked for import");
                this.error = None;
                this.wine.import_from = Some(picked);
                this.pick_executable(cx);
                cx.notify();
            });
        })
        .detach();
    }

    fn prefix_installer_row(&self, cx: &Context<Self>) -> Div {
        let running = matches!(self.installer, Installer::Running);
        // One or the other: an imported prefix replaces the game's own,
        // which an installer has already written into.
        let installed = matches!(self.installer, Installer::Done);
        let status = match (&self.wine.import_from, &self.installer) {
            (Some(picked), _) => theme::muted(match self.wine.import_mode {
                ImportMode::Copy => t!("external.form.import.copy_hint", prefix = picked.display()),
                ImportMode::Move => t!("external.form.import.move_hint", prefix = picked.display()),
            }),
            (None, Installer::Idle) => theme::muted(t!("external.form.installer.hint")),
            (None, Installer::Running) => theme::muted(t!("external.form.installer.running")),
            (None, Installer::Done) => theme::muted(t!("external.form.installer.done")),
            (None, Installer::Failed(err)) => theme::error(err),
        };
        div()
            .flex()
            .items_center()
            .gap(px(10.))
            .mb_3()
            .child(
                button(
                    "external-run-installer",
                    ButtonKind::Secondary,
                    t!("external.form.installer.run"),
                    running,
                )
                .when(!running, |b| {
                    b.on_click(cx.listener(|this, _, _, cx| this.run_installer(cx)))
                }),
            )
            .child(match &self.wine.import_from {
                Some(_) => div()
                    .flex()
                    .gap(px(6.))
                    .child(self.mode_button(ImportMode::Copy, t!("external.form.import.copy"), cx))
                    .child(self.mode_button(ImportMode::Move, t!("external.form.import.move"), cx))
                    .child(
                        button(
                            "external-import-cancel",
                            ButtonKind::Icon,
                            t!("external.form.import.cancel"),
                            false,
                        )
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.wine.import_from = None;
                            cx.notify();
                        })),
                    ),
                None => div().child(
                    button(
                        "external-import-prefix",
                        ButtonKind::Secondary,
                        t!("external.form.import.button"),
                        running || installed,
                    )
                    .when(!running && !installed, |b| {
                        b.on_click(cx.listener(|this, _, _, cx| this.pick_prefix(cx)))
                    }),
                ),
            })
            .child(div().flex_1().min_w_0().child(status))
    }

    pub(super) fn import_progress(&self) -> Div {
        let snapshot = self.wine.progress.snapshot();
        let text = snapshot.map_or_else(
            || t!("external.form.import.measuring"),
            |(copied, total)| {
                t!(
                    "external.form.import.progress",
                    done = crate::format::bytes(copied),
                    total = crate::format::bytes(total),
                    percent = format!("{:.0}", copied as f64 * 100. / total as f64)
                )
            },
        );
        div()
            .flex()
            .flex_col()
            .gap(px(6.))
            .mt_4()
            .max_w(px(520.))
            .child(crate::widgets::progress_bar(
                "external-import-progress",
                snapshot.map(|(copied, total)| copied as f64 / total as f64),
                6.,
            ))
            .child(theme::muted(text).text_size(px(12.)))
    }

    /// Which library folder a new game goes in — only with more than one,
    /// and only until an installer has written into the prefix there.
    pub(super) fn folder_row(&self, cx: &Context<Self>) -> Option<Div> {
        if self.editing || self.prefix_of.is_some() || self.wine.folders.len() < 2 {
            return None;
        }
        let locked = !matches!(self.installer, Installer::Idle | Installer::Failed(_));
        let options = self.wine.folders.iter().map(|(root, free)| {
            let label = match free {
                Some(free) => format!(
                    "{} — {}",
                    root.display(),
                    t!("external.form.folder.free", free = crate::format::bytes(*free))
                ),
                None => root.display().to_string(),
            };
            (
                SharedString::from(root.to_string_lossy().into_owned()),
                SharedString::from(label),
            )
        });
        let main = self.wine.folders.first().map(|(root, _)| root.clone());
        let current = self
            .library_root
            .clone()
            .or_else(|| main.clone())
            .map(|root| SharedString::from(root.to_string_lossy().into_owned()))
            .unwrap_or_default();
        let this = cx.entity().downgrade();
        let picker: AnyElement = if locked {
            theme::muted(current.to_string()).into_any_element()
        } else {
            crate::widgets::select::Select::new("external-folder", options, current)
                .on_change(move |value, _, cx| {
                    let picked = std::path::PathBuf::from(value.as_ref());
                    let root = (Some(&picked) != main.as_ref()).then_some(picked);
                    let _ = this.update(cx, |this, cx| {
                        this.library_root = root;
                        cx.notify();
                    });
                })
                .into_any_element()
        };
        Some(
            div()
                .flex()
                .items_center()
                .gap(px(10.))
                .mb_3()
                .max_w(px(640.))
                .child(
                    div()
                        .text_size(px(12.5))
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(t!("external.form.folder.title")),
                )
                .child(div().flex_1().min_w_0().child(picker)),
        )
    }

    fn mode_button(&self, mode: ImportMode, label: Cow<'static, str>, cx: &Context<Self>) -> impl IntoElement + use<> {
        let kind = if self.wine.import_mode == mode {
            ButtonKind::Primary
        } else {
            ButtonKind::Secondary
        };
        button(("external-import-mode", mode as usize), kind, label, false).on_click(cx.listener(
            move |this, _, _, cx| {
                this.wine.import_mode = mode;
                cx.notify();
            },
        ))
    }
}

/// `profile_id`'s library folders, main first, with each one's free space.
fn library_folders(profile_id: &str) -> Vec<(std::path::PathBuf, Option<u64>)> {
    crate::core::library_for_profile(profile_id)
        .map(|library| library.roots())
        .unwrap_or_default()
        .into_iter()
        .map(|root| {
            let free = decanter_core::disk_usage(&root).map(|(free, _)| free);
            (root, free)
        })
        .collect()
}
