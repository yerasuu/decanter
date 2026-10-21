//! Ajustes of the PS section: where this profile's dumps are read from, the
//! folders of the user's own system module and font dumps, and which psport
//! builds the library runs.

use std::path::PathBuf;

use decanter_ps::bbport::LauncherSettings;
use decanter_ps::channel::Channel;
use decanter_ps::port::Port;
use gpui::{AppContext as _, Context, Entity, SharedString, Window, div, prelude::*};
use rust_i18n::t;

use super::online::OnlineView;
use crate::i18n::Text;
use crate::state::Ps;
use crate::theme;
use crate::widgets::{ButtonKind, button, checkbox};

/// A folder from the user's own console dump, kept under a `LauncherSettings` key that the
/// port gets as an environment variable.
struct DumpFolder {
    key: &'static str,
    title: &'static str,
    none: &'static str,
    description: &'static str,
}

/// `PS_SYS_MODULES` and `PS_SYS_FONTS`, in the order shown.
const DUMP_FOLDERS: [DumpFolder; 2] = [
    DumpFolder {
        key: "sys_modules_dir",
        title: "ps.settings.sys_modules.title",
        none: "ps.settings.sys_modules.none",
        description: "ps.settings.sys_modules.description",
    },
    DumpFolder {
        key: "sys_fonts_dir",
        title: "ps.settings.sys_fonts.title",
        none: "ps.settings.sys_fonts.none",
        description: "ps.settings.sys_fonts.description",
    },
];

pub struct SettingsView {
    ps: Entity<Ps>,
    /// One per `DUMP_FOLDERS`, read off the UI thread when the view opens,
    /// after each change and whenever `Ps` changes.
    dump_folders: [Option<String>; 2],
    channel: Option<Channel>,
    online: Entity<OnlineView>,
    error: Option<Text>,
}

impl SettingsView {
    /// Watches `ps` since the games folder follows the active profile's
    /// library.
    pub fn new(ps: Entity<Ps>, cx: &mut Context<Self>) -> Self {
        cx.observe(&ps, |this, _, cx| this.refresh(cx)).detach();
        let this = Self {
            ps,
            dump_folders: [None, None],
            channel: None,
            online: cx.new(OnlineView::new),
            error: None,
        };
        this.refresh(cx);
        this
    }

    fn refresh(&self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            let (dirs, channel) = cx
                .background_spawn(async {
                    let root = decanter_ps::paths::active();
                    let settings = LauncherSettings::load(&root);
                    let dirs = DUMP_FOLDERS.map(|folder| settings.text(folder.key).map(str::to_owned));
                    (dirs, Channel::of(&root, Port::Bbport))
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.dump_folders = dirs;
                this.channel = Some(channel);
                cx.notify();
            });
        })
        .detach();
    }

    /// `key` of one of `DUMP_FOLDERS`; `None` forgets the folder.
    fn set_dump_folder(&self, key: &'static str, dir: Option<String>, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            let saved = cx
                .background_spawn(async move {
                    LauncherSettings::edit(&decanter_ps::paths::active(), |settings| match &dir {
                        Some(dir) => settings.set_text(key, dir),
                        None => settings.remove(key),
                    })
                })
                .await;
            let _ = this.update(cx, |this, cx| match saved {
                Ok(()) => {
                    this.error = None;
                    this.refresh(cx);
                }
                Err(err) => {
                    this.error = Some(Text::new("ps.settings.save_error").arg("error", err));
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
                    this.error = Some(Text::new("ps.settings.channel.save_error").arg("error", err));
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

    fn pick_dump_folder(&self, key: &'static str, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            let Some(folder) = rfd::AsyncFileDialog::new().pick_folder().await else {
                return;
            };
            let dir = folder.path().to_string_lossy().into_owned();
            let _ = this.update(cx, |this, cx| this.set_dump_folder(key, Some(dir), cx));
        })
        .detach();
    }

    /// `index` only tells the section's elements apart.
    fn dump_folder_section(
        index: usize,
        folder: &'static DumpFolder,
        dir: Option<String>,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let key = folder.key;
        let has_dir = dir.is_some();
        div()
            .flex()
            .flex_col()
            .gap_4()
            .child(theme::h2(t!(folder.title)))
            .child(
                theme::card().child(
                    theme::list_row(("ps-dump-folder", index), true)
                        .child(
                            theme::mono()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .child(dir.unwrap_or_else(|| t!(folder.none).into_owned())),
                        )
                        .child(
                            button(
                                ("ps-dump-folder-pick", index),
                                ButtonKind::Secondary,
                                t!("ps.settings.pick"),
                                false,
                            )
                            .on_click(cx.listener(move |this, _, _, cx| this.pick_dump_folder(key, cx))),
                        )
                        .when(has_dir, |row| {
                            row.child(
                                button(
                                    ("ps-dump-folder-forget", index),
                                    ButtonKind::Secondary,
                                    t!("ps.settings.forget"),
                                    false,
                                )
                                .on_click(cx.listener(move |this, _, _, cx| this.set_dump_folder(key, None, cx))),
                            )
                        }),
                ),
            )
            .child(theme::muted(t!(folder.description)))
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
                .children(
                    DUMP_FOLDERS
                        .iter()
                        .zip(self.dump_folders.clone())
                        .enumerate()
                        .map(|(index, (folder, dir))| Self::dump_folder_section(index, folder, dir, cx)),
                )
                .child(theme::h2(t!("ps.settings.channel.title")))
                .child(
                    theme::card()
                        .child(self.channel_row(Channel::Release, false, cx))
                        .child(self.channel_row(Channel::Testing, true, cx)),
                )
                .child(self.online.clone())
                .children(self.error.as_ref().map(theme::error)),
        )
    }
}
