//! Debug mode in Ajustes › General: the switch, the folder its logs go to and a way to open it
//! (see [`crate::debug_logs`]).

use std::path::PathBuf;

use gpui::{Context, IntoElement, Window, div, prelude::*};
use rust_i18n::t;

use super::{column, field_row};
use crate::i18n::Text;
use crate::theme;
use crate::widgets::{ButtonKind, button, checkbox};

pub struct DebugSection {
    enabled: bool,
    /// The folder and whether the user picked it; `None` without a library for the default.
    folder: Option<(PathBuf, bool)>,
    error: Option<Text>,
}

impl DebugSection {
    pub fn new(_: &mut Context<Self>) -> Self {
        Self {
            enabled: crate::debug_logs::enabled(),
            folder: crate::debug_logs::folder(),
            error: None,
        }
    }

    fn set_enabled(&mut self, enabled: bool, cx: &mut Context<Self>) {
        match crate::debug_logs::set_enabled(enabled) {
            Ok(()) => {
                tracing::info!(enabled, "debug mode changed");
                self.enabled = enabled;
                self.error = None;
            }
            Err(err) => {
                tracing::warn!(enabled, %err, "could not save the debug mode");
                self.error = Some(err);
            }
        }
        cx.notify();
    }

    /// Asks for a folder with `pick`, or goes back to the default without.
    fn change_folder(&mut self, pick: bool, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            let picked = if pick {
                let Some(picked) = rfd::AsyncFileDialog::new()
                    .set_title(t!("settings.general.debug.pick_title"))
                    .pick_folder()
                    .await
                else {
                    return;
                };
                Some(picked.path().to_path_buf())
            } else {
                None
            };
            let result = cx
                .background_spawn(async move { crate::debug_logs::set_folder(picked.as_deref()) })
                .await;
            let _ = this.update(cx, |this, cx| {
                match result {
                    Ok(()) => {
                        this.folder = crate::debug_logs::folder();
                        tracing::info!(
                            folder = this
                                .folder
                                .as_ref()
                                .map(|(dir, _)| tracing::field::display(dir.display())),
                            "debug log folder changed"
                        );
                        this.error = None;
                    }
                    Err(err) => this.error = Some(err),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn open_folder(&mut self, folder: PathBuf, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            let opened = cx
                .background_spawn({
                    let folder = folder.clone();
                    async move { crate::debug_logs::open_folder(&folder) }
                })
                .await;
            if let Err(err) = opened {
                tracing::warn!(folder = %folder.display(), %err, "could not open the debug log folder");
                let _ = this.update(cx, |this, cx| {
                    this.error = Some(Text::new("settings.general.debug.errors.open").arg("error", err));
                    cx.notify();
                });
            }
        })
        .detach();
    }
}

impl Render for DebugSection {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let enabled = self.enabled;
        // The app's log is set up before the window, so a switch flipped now reaches it next start.
        let pending = (enabled != crate::logs::debug_session()).then(|| {
            theme::muted(if enabled {
                t!("settings.general.debug.app_log_next_start")
            } else {
                t!("settings.general.debug.app_log_until_exit")
            })
        });
        let folder = self.folder.clone().map(|(dir, chosen)| {
            let open = dir.clone();
            column()
                .child(div().child(t!("settings.general.debug.folder")))
                .child(theme::mono().child(dir.display().to_string()))
                .child(theme::muted(if chosen {
                    t!("settings.general.debug.folder_chosen")
                } else {
                    t!("settings.general.debug.folder_default")
                }))
                .child(theme::muted(t!("settings.general.debug.folder_scrubbed")))
                .child(
                    field_row()
                        .mt_2()
                        .child(
                            button(
                                "debug-folder-change",
                                ButtonKind::Secondary,
                                t!("settings.general.debug.change"),
                                false,
                            )
                            .on_click(cx.listener(|this, _, _, cx| this.change_folder(true, cx))),
                        )
                        .when(chosen, |row| {
                            row.child(
                                button(
                                    "debug-folder-reset",
                                    ButtonKind::Secondary,
                                    t!("settings.general.debug.reset"),
                                    false,
                                )
                                .on_click(cx.listener(|this, _, _, cx| this.change_folder(false, cx))),
                            )
                        })
                        .child(
                            button(
                                "debug-folder-open",
                                ButtonKind::Secondary,
                                t!("settings.general.debug.open"),
                                false,
                            )
                            .on_click(cx.listener(move |this, _, _, cx| this.open_folder(open.clone(), cx))),
                        ),
                )
        });
        column()
            .child(theme::h2(t!("settings.general.debug.title")))
            .child(
                field_row()
                    .id("debug-mode")
                    .items_center()
                    .cursor_pointer()
                    .on_click(cx.listener(move |this, _, _, cx| this.set_enabled(!enabled, cx)))
                    .child(checkbox(enabled))
                    .child(div().min_w_0().child(t!("settings.general.debug.enabled"))),
            )
            .child(theme::muted(t!("settings.general.debug.hint")))
            .children(pending)
            .children(folder)
            .children(self.error.as_ref().map(theme::error))
    }
}
