//! The UI language, window behaviour that isn't tied to any profile or
//! game, closing a game from the controller or keyboard, the Steam export
//! toggle and its pending-restart callout, and whether GitHub has a newer
//! decanter, which an AppImage can install over itself.

use std::path::PathBuf;

use decanter_core::capabilities::Feature;
use gpui::{AnyElement, App, Context, IntoElement, SharedString, Window, div, prelude::*};
use rust_i18n::t;

use super::{column, field_row, inline};
use crate::capabilities::Caps;
use crate::i18n::{self, Language};
use crate::releases::{self, Release, ReleaseError, Standing};
use crate::self_update::{self, Availability, InstallError};
use crate::theme;
use crate::widgets::{ButtonKind, button, checkbox, language_select};

pub struct GeneralTab {
    language: Option<Language>,
    close_to_tray: bool,
    close_game_shortcut: bool,
    steam_export_enabled: bool,
    steam_pending: bool,
    restarting_steam: bool,
    restart_error: Option<String>,
    checking_release: bool,
    /// `None` until the user checks; never checked on its own.
    release: Option<Result<Standing, ReleaseError>>,
    availability: Availability,
    install: Option<Install>,
}

enum Install {
    Downloading { done: u64, total: Option<u64> },
    Installed { tag: String, appimage: PathBuf },
    Failed(InstallError),
}

impl GeneralTab {
    pub fn new(_: &mut Context<Self>) -> Self {
        let mut this = Self {
            language: None,
            close_to_tray: false,
            close_game_shortcut: true,
            steam_export_enabled: true,
            steam_pending: false,
            restarting_steam: false,
            restart_error: None,
            checking_release: false,
            release: None,
            availability: self_update::availability(),
            install: None,
        };
        this.reload();
        this
    }

    fn reload(&mut self) {
        self.language = i18n::saved();
        self.close_to_tray = crate::core::db()
            .and_then(|db| db.close_to_tray().map_err(crate::core::StoreError::Query))
            .inspect_err(
                |err| tracing::warn!(error = %err, "failed to read close_to_tray setting, defaulting to false"),
            )
            .unwrap_or(false);
        self.close_game_shortcut = crate::closing::enabled();
        self.steam_export_enabled = crate::core::db()
            .and_then(|db| db.steam_export_enabled().map_err(crate::core::StoreError::Query))
            .inspect_err(
                |err| tracing::warn!(error = %err, "failed to read steam_export_enabled setting, defaulting to true"),
            )
            .unwrap_or(true);
        self.steam_pending = crate::core::steam_shortcuts_pending();
    }

    fn set_language(&mut self, language: Option<Language>, cx: &mut Context<Self>) {
        i18n::set(language, cx);
        self.reload();
        cx.notify();
    }

    fn set_close_to_tray(&mut self, close_to_tray: bool, cx: &mut Context<Self>) {
        match crate::core::db() {
            Ok(db) => match db.set_close_to_tray(close_to_tray) {
                Ok(()) => {
                    tracing::info!(close_to_tray, "close-to-tray setting changed");
                    self.reload();
                    cx.notify();
                }
                Err(err) => tracing::warn!(close_to_tray, error = %err, "failed to save close_to_tray setting"),
            },
            Err(err) => {
                tracing::warn!(close_to_tray, error = %err, "failed to open db, close_to_tray setting not saved");
            }
        }
    }

    fn set_close_game_shortcut(&mut self, enabled: bool, cx: &mut Context<Self>) {
        match crate::core::db() {
            Ok(db) => match db.set_close_game_shortcut_enabled(enabled) {
                Ok(()) => {
                    tracing::info!(enabled, "close-game shortcut setting changed");
                    crate::closing::sync(cx);
                    self.reload();
                    cx.notify();
                }
                Err(err) => tracing::warn!(enabled, error = %err, "failed to save close_game_shortcut setting"),
            },
            Err(err) => {
                tracing::warn!(enabled, error = %err, "failed to open db, close_game_shortcut setting not saved");
            }
        }
    }

    fn set_steam_export(&mut self, enabled: bool, cx: &mut Context<Self>) {
        match crate::core::db() {
            Ok(db) => match db.set_steam_export_enabled(enabled) {
                Ok(()) => {
                    tracing::info!(enabled, "steam export setting changed");
                    self.reload();
                    cx.notify();
                }
                Err(err) => tracing::warn!(enabled, error = %err, "failed to save steam_export_enabled setting"),
            },
            Err(err) => {
                tracing::warn!(enabled, error = %err, "failed to open db, steam_export_enabled setting not saved");
            }
        }
    }

    fn restart_steam(&mut self, cx: &mut Context<Self>) {
        self.restarting_steam = true;
        self.restart_error = None;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let outcome = crate::core::restart_steam_to_apply_pending().await;
            let _ = this.update(cx, |this, cx| {
                match outcome {
                    Ok(()) => {
                        tracing::info!("steam restarted to apply pending shortcuts");
                        this.reload();
                    }
                    Err(err) => {
                        tracing::warn!(error = %err, "failed to restart steam");
                        this.restart_error = Some(err);
                    }
                }
                this.restarting_steam = false;
                cx.notify();
            });
        })
        .detach();
    }

    fn check_release(&mut self, cx: &mut Context<Self>) {
        self.checking_release = true;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let outcome = releases::decanter().await;
            if let Err(err) = &outcome {
                tracing::warn!(error = %err, "could not check decanter's latest release");
            }
            let _ = this.update(cx, |this, cx| {
                this.release = Some(outcome);
                this.checking_release = false;
                cx.notify();
            });
        })
        .detach();
    }

    fn install_release(&mut self, release: &Release, appimage: PathBuf, cx: &mut Context<Self>) {
        self.install = Some(Install::Downloading { done: 0, total: None });
        cx.notify();
        let tag = release.tag.clone();
        cx.spawn(async move |this, cx| {
            let outcome = self_update::install(&tag, &appimage, |done, total| {
                let _ = this.update(cx, |this, cx| {
                    this.install = Some(Install::Downloading { done, total });
                    cx.notify();
                });
            })
            .await;
            let _ = this.update(cx, |this, cx| {
                this.install = Some(match outcome {
                    Ok(()) => Install::Installed { tag, appimage },
                    Err(err) => Install::Failed(err),
                });
                cx.notify();
            });
        })
        .detach();
    }

    /// What a newer release offers: the button that installs it over this
    /// AppImage and how far that got, or why it can't be offered here.
    fn install_section(&self, release: &Release, cx: &Context<Self>) -> impl IntoElement {
        let appimage = match &self.availability {
            Availability::AppImage(appimage) => appimage,
            Availability::NoKey => {
                return column()
                    .child(inline(button(
                        "install-decanter-release",
                        ButtonKind::Primary,
                        t!("settings.general.updates.install"),
                        true,
                    )))
                    .child(theme::muted(t!("settings.general.updates.no_key")));
            }
            Availability::Elsewhere => return column(),
        };
        if let Some(Install::Installed { tag, appimage }) = &self.install {
            let appimage = appimage.clone();
            return column()
                .child(div().child(t!("settings.general.updates.installed_update", tag = tag)))
                .child(inline(
                    button(
                        "restart-decanter",
                        ButtonKind::Primary,
                        t!("settings.general.updates.restart_now"),
                        false,
                    )
                    .on_click(move |_, _, cx| {
                        self_update::restart_after_quit(appimage.clone());
                        cx.quit();
                    }),
                ));
        }
        let downloading = match &self.install {
            Some(Install::Downloading { done, total }) => Some(match total {
                Some(total) => t!(
                    "settings.general.updates.downloading_of",
                    done = crate::format::bytes(*done),
                    total = crate::format::bytes(*total)
                ),
                None => t!(
                    "settings.general.updates.downloading",
                    done = crate::format::bytes(*done)
                ),
            }),
            None | Some(Install::Installed { .. } | Install::Failed(_)) => None,
        };
        let error = match &self.install {
            Some(Install::Failed(err)) => Some(err.localized()),
            None | Some(Install::Downloading { .. } | Install::Installed { .. }) => None,
        };
        let busy = downloading.is_some();
        let release = release.clone();
        let appimage = appimage.clone();
        column()
            .child(inline(
                button(
                    "install-decanter-release",
                    ButtonKind::Primary,
                    t!("settings.general.updates.install"),
                    busy,
                )
                .when(!busy, |b| {
                    b.on_click(cx.listener(move |this, _, _, cx| {
                        this.install_release(&release, appimage.clone(), cx);
                    }))
                }),
            ))
            .children(downloading.map(theme::muted))
            .children(error.map(theme::error))
    }

    fn release_section(&self, cx: &Context<Self>) -> impl IntoElement {
        let checking = self.checking_release;
        let installable = !matches!(self.availability, Availability::Elsewhere);
        let newer = match &self.release {
            Some(Ok(Standing::Newer(release))) => Some(release),
            None | Some(Ok(Standing::UpToDate(_) | Standing::Ahead(_) | Standing::NoRelease) | Err(_)) => None,
        };
        let (text, page) = match &self.release {
            Some(Ok(Standing::UpToDate(release))) => {
                (Some(t!("settings.general.updates.up_to_date", tag = release.tag)), None)
            }
            Some(Ok(Standing::Ahead(release))) => (Some(t!("settings.general.updates.ahead", tag = release.tag)), None),
            Some(Ok(Standing::Newer(release))) if installable => (
                Some(t!("settings.general.updates.available_install", tag = release.tag)),
                Some(release.url.clone()),
            ),
            Some(Ok(Standing::Newer(release))) => (
                Some(t!("settings.general.updates.available", tag = release.tag)),
                Some(release.url.clone()),
            ),
            Some(Ok(Standing::NoRelease)) => (Some(t!("settings.general.updates.none")), None),
            None | Some(Err(_)) => (None, None),
        };
        let error = match &self.release {
            Some(Err(err)) => Some(err.localized()),
            None | Some(Ok(_)) => None,
        };
        column()
            .child(theme::muted(t!(
                "settings.general.updates.installed",
                version = decanter_core::BUILD
            )))
            .child(
                field_row()
                    .mt_2()
                    .child(
                        button(
                            "check-decanter-release",
                            ButtonKind::Secondary,
                            if checking {
                                t!("settings.general.updates.checking")
                            } else {
                                t!("settings.general.updates.check")
                            },
                            checking,
                        )
                        .when(!checking, |b| {
                            b.on_click(cx.listener(|this, _, _, cx| this.check_release(cx)))
                        }),
                    )
                    .children(page.map(|url| {
                        button(
                            "open-release-notes",
                            ButtonKind::Secondary,
                            t!("settings.general.updates.notes"),
                            false,
                        )
                        .on_click(move |_, _, _| {
                            if let Err(err) = crate::desktop::open(&url) {
                                tracing::warn!(url, %err, "could not open the release page");
                            }
                        })
                    })),
            )
            .children(text.map(|text| div().child(text)))
            .children(newer.map(|release| self.install_section(release, cx)))
            .children(error.map(theme::error))
    }

    /// `label.field-row` around a checkbox: the whole row toggles it, the
    /// way clicking a `<label>`'s text does.
    fn checkbox_row(
        id: &'static str,
        checked: bool,
        text: impl Into<SharedString>,
        on_toggle: impl Fn(&mut Self, bool, &mut Context<Self>) + 'static,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        field_row()
            .id(id)
            .items_center()
            .cursor_pointer()
            .on_click(cx.listener(move |this, _, _, cx| on_toggle(this, !checked, cx)))
            .child(checkbox(checked))
            .child(div().min_w_0().child(text.into()))
    }

    fn language_select(&self, cx: &Context<Self>) -> impl IntoElement {
        let entity = cx.entity();
        field_row().child(language_select(self.language, move |language, _, cx| {
            entity.update(cx, |this, cx| this.set_language(language, cx));
        }))
    }
}

impl Render for GeneralTab {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let restarting = self.restarting_steam;
        let callout = self.steam_pending.then(|| {
            column()
                .child(div().child(t!("settings.general.steam.pending")))
                .child(inline(
                    button(
                        "restart-steam",
                        ButtonKind::Primary,
                        if restarting {
                            t!("settings.general.steam.restarting")
                        } else {
                            t!("settings.general.steam.restart_now")
                        },
                        restarting,
                    )
                    .when(!restarting, |b| {
                        b.on_click(cx.listener(|this, _, _, cx| this.restart_steam(cx)))
                    }),
                ))
                .children(self.restart_error.clone().map(theme::error))
        });

        column()
            .child(theme::h2(t!("settings.general.language.title")))
            .child(self.language_select(cx))
            .child(theme::h2(t!("settings.general.close.title")))
            .child(offered(
                Feature::CloseToTray,
                column()
                    .child(Self::checkbox_row(
                        "close-to-tray",
                        self.close_to_tray,
                        t!("settings.general.close.to_tray"),
                        Self::set_close_to_tray,
                        cx,
                    ))
                    .child(theme::muted(t!("settings.general.close.restart_hint"))),
                cx,
            ))
            .child(theme::h2(t!("settings.general.close_game.title")))
            .child(offered(
                Feature::PadClose,
                column()
                    .child(Self::checkbox_row(
                        "close-game-shortcut",
                        self.close_game_shortcut,
                        t!("settings.general.close_game.enabled"),
                        Self::set_close_game_shortcut,
                        cx,
                    ))
                    .child(theme::muted(t!(
                        "settings.general.close_game.hint",
                        hold = crate::closing::HOLD.as_secs(),
                        grace = crate::closing::GRACE.as_secs()
                    ))),
                cx,
            ))
            .child(theme::h2(t!("settings.general.steam.title")))
            .child(offered(
                Feature::SteamExport,
                column()
                    .child(Self::checkbox_row(
                        "steam-export",
                        self.steam_export_enabled,
                        t!("settings.general.steam.add"),
                        Self::set_steam_export,
                        cx,
                    ))
                    .child(theme::muted(t!("settings.general.steam.restart_hint")))
                    .children(callout),
                cx,
            ))
            .child(theme::h2(t!("settings.general.updates.title")))
            .child(self.release_section(cx))
    }
}

/// A section's controls, or why they aren't there. Its arguments are built
/// before it borrows `cx`, which the controls' listeners need too.
fn offered(feature: Feature, controls: impl IntoElement, cx: &App) -> AnyElement {
    Caps::get(cx).gate_or_reason(feature, controls)
}
