use std::borrow::Cow;

use gpui::{AnyElement, Context, Div, FontWeight, SharedString, div, prelude::*, px};
use rust_i18n::t;

use super::{GameForm, Installer};
use crate::external::import::Mode as ImportMode;
use crate::theme;
use crate::widgets::{ButtonKind, button};

impl GameForm {
    fn run_installer(&mut self, cx: &mut Context<Self>) {
        let (profile_id, id, proton) = (self.profile_id.clone(), self.id.clone(), self.proton.clone());
        let library_root = self.library_root.clone();
        self._work = Some(cx.spawn(async move |this, cx| {
            let Some(file) = rfd::AsyncFileDialog::new()
                .set_title(t!("external.form.installer.pick_title"))
                .add_filter(t!("external.form.installer.filter"), &["exe"])
                .pick_file()
                .await
            else {
                return;
            };
            let installer = file.path().to_path_buf();
            let _ = this.update(cx, |this, cx| {
                this.installer = Installer::Running;
                this.error.clear();
                cx.notify();
            });
            let result =
                crate::external::run_installer(&profile_id, &id, &installer, proton, library_root.as_deref()).await;
            let _ = this.update(cx, |this, cx| {
                this.installer = match result {
                    Ok(()) => Installer::Done,
                    Err(err) => Installer::Failed(err),
                };
                cx.notify();
                if matches!(this.installer, Installer::Done) {
                    this.pick_executable(cx);
                }
            });
        }));
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
                    this.error = t!("external.errors.not_a_prefix").into_owned();
                    cx.notify();
                    return;
                }
                tracing::debug!(prefix = %picked.display(), "prefix picked for import");
                this.error.clear();
                this.import_from = Some(picked);
                this.pick_executable(cx);
                cx.notify();
            });
        })
        .detach();
    }

    pub(super) fn installer_row(&self, cx: &Context<Self>) -> Div {
        let running = matches!(self.installer, Installer::Running);
        // One or the other: an imported prefix replaces the game's own,
        // which an installer has already written into.
        let installed = matches!(self.installer, Installer::Done);
        let status = match (&self.import_from, &self.installer) {
            (Some(picked), _) => theme::muted(match self.import_mode {
                ImportMode::Copy => t!("external.form.import.copy_hint", prefix = picked.display()),
                ImportMode::Move => t!("external.form.import.move_hint", prefix = picked.display()),
            }),
            (None, Installer::Idle) => theme::muted(t!("external.form.installer.hint")),
            (None, Installer::Running) => theme::muted(t!("external.form.installer.running")),
            (None, Installer::Done) => theme::muted(t!("external.form.installer.done")),
            (None, Installer::Failed(err)) => theme::error(err.clone()),
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
            .child(match &self.import_from {
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
                            this.import_from = None;
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
        let snapshot = self.progress.snapshot();
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
        if self.editing || self.prefix_of.is_some() || self.folders.len() < 2 {
            return None;
        }
        let locked = !matches!(self.installer, Installer::Idle | Installer::Failed(_));
        let options = self.folders.iter().map(|(root, free)| {
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
        let main = self.folders.first().map(|(root, _)| root.clone());
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
        let kind = if self.import_mode == mode {
            ButtonKind::Primary
        } else {
            ButtonKind::Secondary
        };
        button(("external-import-mode", mode as usize), kind, label, false).on_click(cx.listener(
            move |this, _, _, cx| {
                this.import_mode = mode;
                cx.notify();
            },
        ))
    }
}
