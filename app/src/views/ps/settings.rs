//! Ajustes of the PS section: where this profile's dumps are read from, and
//! the folder of the user's own system module dump.

use decanter_ps::bbport::LauncherSettings;
use gpui::{AppContext as _, Context, Entity, Window, div, prelude::*};
use rust_i18n::t;

use crate::state::Ps;
use crate::theme;
use crate::widgets::{ButtonKind, button};

/// `LauncherSettings` key; passed to bbport as `BB_SYS_MODULES`.
const SYS_MODULES: &str = "sys_modules_dir";

pub struct SettingsView {
    /// Read off the UI thread when the view opens, after each change and
    /// whenever `Ps` changes.
    sys_modules: Option<String>,
    error: Option<String>,
}

impl SettingsView {
    /// Watches `ps` since the games folder follows the active profile's
    /// library.
    pub fn new(ps: Entity<Ps>, cx: &mut Context<Self>) -> Self {
        cx.observe(&ps, |this, _, cx| this.refresh(cx)).detach();
        let this = Self {
            sys_modules: None,
            error: None,
        };
        this.refresh(cx);
        this
    }

    fn refresh(&self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            let dir = cx
                .background_spawn(async {
                    LauncherSettings::load(&decanter_ps::paths::active())
                        .text(SYS_MODULES)
                        .map(str::to_owned)
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.sys_modules = dir;
                cx.notify();
            });
        })
        .detach();
    }

    /// `None` forgets the folder.
    fn set_sys_modules(&self, dir: Option<String>, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            let saved = cx
                .background_spawn(async move {
                    LauncherSettings::edit(&decanter_ps::paths::active(), |settings| match &dir {
                        Some(dir) => settings.set_text(SYS_MODULES, dir),
                        None => settings.remove(SYS_MODULES),
                    })
                })
                .await;
            let _ = this.update(cx, |this, cx| match saved {
                Ok(()) => {
                    this.error = None;
                    this.refresh(cx);
                }
                Err(err) => {
                    this.error = Some(t!("ps.settings.save_error", error = err).into_owned());
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn pick_sys_modules(&self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            let Some(folder) = rfd::AsyncFileDialog::new().pick_folder().await else {
                return;
            };
            let dir = folder.path().to_string_lossy().into_owned();
            let _ = this.update(cx, |this, cx| this.set_sys_modules(Some(dir), cx));
        })
        .detach();
    }
}

impl Render for SettingsView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let games = decanter_ps::paths::active().games();
        let sys_modules = self.sys_modules.clone();
        let has_sys_modules = sys_modules.is_some();
        theme::page(
            "ps-settings",
            div()
                .flex()
                .flex_col()
                .gap_4()
                .child(theme::h2(t!("ps.settings.games.title")))
                .child(
                    theme::card().child(
                        theme::list_row("ps-dir-library", true)
                            .child(
                                theme::mono()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .child(games.display().to_string()),
                            )
                            .child(
                                button("ps-dir-open", ButtonKind::Secondary, t!("ps.settings.open"), false).on_click(
                                    move |_, _, _| {
                                        let _ = std::fs::create_dir_all(&games);
                                        if let Err(err) = crate::desktop::open(&games) {
                                            tracing::warn!(dir = %games.display(), %err, "could not open the folder");
                                        }
                                    },
                                ),
                            ),
                    ),
                )
                .child(theme::muted(t!("ps.settings.games.description")))
                .child(theme::h2(t!("ps.settings.sys_modules.title")))
                .child(
                    theme::card().child(
                        theme::list_row("ps-sys-modules", true)
                            .child(
                                theme::mono().flex_1().min_w_0().truncate().child(
                                    sys_modules.unwrap_or_else(|| t!("ps.settings.sys_modules.none").into_owned()),
                                ),
                            )
                            .child(
                                button(
                                    "ps-sys-modules-pick",
                                    ButtonKind::Secondary,
                                    t!("ps.settings.pick"),
                                    false,
                                )
                                .on_click(cx.listener(|this, _, _, cx| this.pick_sys_modules(cx))),
                            )
                            .when(has_sys_modules, |row| {
                                row.child(
                                    button(
                                        "ps-sys-modules-forget",
                                        ButtonKind::Secondary,
                                        t!("ps.settings.forget"),
                                        false,
                                    )
                                    .on_click(cx.listener(|this, _, _, cx| this.set_sys_modules(None, cx))),
                                )
                            }),
                    ),
                )
                .child(theme::muted(t!("ps.settings.sys_modules.description")))
                .children(self.error.clone().map(theme::error)),
        )
    }
}
