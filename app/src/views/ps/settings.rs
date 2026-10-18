//! Ajustes of the PS section: where this profile's dumps are read from, the
//! folder of the user's own system module dump, and which psport builds the
//! library runs.

use std::path::PathBuf;

use decanter_ps::bbport::LauncherSettings;
use decanter_ps::channel::Channel;
use decanter_ps::port::Port;
use gpui::{AppContext as _, Context, Entity, SharedString, Window, div, prelude::*};
use rust_i18n::t;

use crate::state::Ps;
use crate::theme;
use crate::widgets::{ButtonKind, button, checkbox};

/// `LauncherSettings` key; passed to bbport as `PS_SYS_MODULES`.
const SYS_MODULES: &str = "sys_modules_dir";

pub struct SettingsView {
    ps: Entity<Ps>,
    /// Read off the UI thread when the view opens, after each change and
    /// whenever `Ps` changes.
    sys_modules: Option<String>,
    channel: Option<Channel>,
    error: Option<String>,
}

impl SettingsView {
    /// Watches `ps` since the games folder follows the active profile's
    /// library.
    pub fn new(ps: Entity<Ps>, cx: &mut Context<Self>) -> Self {
        cx.observe(&ps, |this, _, cx| this.refresh(cx)).detach();
        let this = Self {
            ps,
            sys_modules: None,
            channel: None,
            error: None,
        };
        this.refresh(cx);
        this
    }

    fn refresh(&self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            let (dir, channel) = cx
                .background_spawn(async {
                    let root = decanter_ps::paths::active();
                    let dir = LauncherSettings::load(&root).text(SYS_MODULES).map(str::to_owned);
                    (dir, Channel::of(&root, Port::Bbport))
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.sys_modules = dir;
                this.channel = Some(channel);
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

    fn set_channel(&self, channel: Channel, cx: &mut Context<Self>) {
        if self.channel == Some(channel) {
            return;
        }
        cx.spawn(async move |this, cx| {
            let saved = cx
                .background_spawn(async move { channel.save(&decanter_ps::paths::active()) })
                .await;
            let _ = this.update(cx, |this, cx| match saved {
                Ok(()) => {
                    tracing::info!(?channel, "psport channel changed");
                    this.error = None;
                    this.channel = Some(channel);
                    this.ps.update(cx, |ps, cx| ps.switch_channel(Port::Bbport, cx));
                    cx.notify();
                }
                Err(err) => {
                    tracing::warn!(?channel, %err, "could not save the psport channel");
                    this.error = Some(t!("ps.settings.channel.save_error", error = err).into_owned());
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn channel_row(&self, channel: Channel, last: bool, cx: &Context<Self>) -> impl IntoElement {
        let (id, title, description) = match channel {
            Channel::Release => (
                "ps-channel-release",
                t!("ps.settings.channel.release.title"),
                t!("ps.settings.channel.release.description"),
            ),
            Channel::Testing => (
                "ps-channel-testing",
                t!("ps.settings.channel.testing.title"),
                t!("ps.settings.channel.testing.description"),
            ),
        };
        theme::list_row(id, last)
            .cursor_pointer()
            .on_click(cx.listener(move |this, _, _, cx| this.set_channel(channel, cx)))
            .child(checkbox(self.channel == Some(channel)))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .min_w_0()
                    .child(SharedString::from(title))
                    .child(theme::muted(description)),
            )
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
        let roots = self.ps.read(cx).library_roots();
        let folders: Vec<PathBuf> = if roots.is_empty() {
            vec![decanter_ps::paths::active().games()]
        } else {
            roots.iter().map(|root| decanter_ps::paths::games_in(root)).collect()
        };
        let sys_modules = self.sys_modules.clone();
        let has_sys_modules = sys_modules.is_some();
        let count = folders.len();
        theme::page(
            "ps-settings",
            div()
                .flex()
                .flex_col()
                .gap_4()
                .child(theme::h2(t!("ps.settings.games.title")))
                .child(
                    theme::card().children(folders.into_iter().enumerate().map(|(index, games)| {
                        let label = if index == 0 {
                            t!("storage.folder.primary")
                        } else {
                            t!("storage.folder.extra")
                        };
                        theme::list_row(("ps-dir-library", index), index + 1 == count)
                            .child(theme::badge(label, index == 0))
                            .child(
                                theme::mono()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .child(games.display().to_string()),
                            )
                            .child(
                                button(
                                    ("ps-dir-open", index),
                                    ButtonKind::Secondary,
                                    t!("ps.settings.open"),
                                    false,
                                )
                                .on_click(move |_, _, _| {
                                    let _ = std::fs::create_dir_all(&games);
                                    if let Err(err) = crate::desktop::open(&games) {
                                        tracing::warn!(dir = %games.display(), %err, "could not open the folder");
                                    }
                                }),
                            )
                    })),
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
                .child(theme::h2(t!("ps.settings.channel.title")))
                .child(
                    theme::card()
                        .child(self.channel_row(Channel::Release, false, cx))
                        .child(self.channel_row(Channel::Testing, true, cx)),
                )
                .children(self.error.clone().map(theme::error)),
        )
    }
}
