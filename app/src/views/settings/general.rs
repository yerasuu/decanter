//! The UI language, window behaviour that isn't tied to any profile or
//! game, closing a game from the controller or keyboard, the Steam
//! compatibility switches and their pending-restart callout, whether
//! downloads carry on by themselves at the next start, whether GitHub has a newer
//! decanter, which an AppImage can install over itself or into decanter's
//! own copy, the user's Nexus Mods key, and debug mode.

use std::path::{Path, PathBuf};

use decanter_core::capabilities::Feature;
use gpui::{AnyElement, App, Context, Entity, IntoElement, SharedString, Window, div, prelude::*, px};
use rust_i18n::t;

use super::credentials::CredentialsSection;
use super::debug::DebugSection;
use super::modio::ModioKeySection;
use super::nexus::NexusSection;
use super::{Tab, column, field_row, inline, section};
use crate::capabilities::Caps;
use crate::i18n::{self, Language, Text};
use crate::platform;
use crate::releases::{self, Release, ReleaseError, Standing};
use crate::self_update::{self, Availability, InstallError};
use crate::state::AppState;
use crate::theme;
use crate::widgets::{ButtonKind, SettingRow, button, language_note, language_select};

/// Hidden until uploading Galaxy achievements exists; the setting and its code stay.
const GALAXY_UPLOAD_AVAILABLE: bool = false;

pub struct GeneralTab {
    tab: Tab,
    language: Option<Language>,
    close_to_tray: bool,
    close_game_shortcut: bool,
    steam_compat: bool,
    steam_register_folders: bool,
    steam_export_enabled: bool,
    steam_discover: bool,
    resume_downloads: bool,
    galaxy_service: bool,
    galaxy_upload: bool,
    raven: bool,
    steam_pending: bool,
    restarting_steam: bool,
    steam_needs_close_confirmation: bool,
    restart_error: Option<Text>,
    checking_release: bool,
    /// `None` until the user checks; never checked on its own.
    release: Option<Result<Standing, ReleaseError>>,
    availability: Availability,
    install: Option<Install>,
    modio: Entity<ModioKeySection>,
    nexus: Entity<NexusSection>,
    credentials: Entity<CredentialsSection>,
    debug: Entity<DebugSection>,
}

enum Install {
    Downloading { done: u64, total: Option<u64> },
    Installed { tag: String, appimage: PathBuf },
    Failed(InstallError),
}

impl GeneralTab {
    pub fn new(tab: Tab, cx: &mut Context<Self>) -> Self {
        let mut this = Self {
            tab,
            language: None,
            close_to_tray: false,
            close_game_shortcut: true,
            steam_compat: true,
            steam_register_folders: true,
            steam_export_enabled: true,
            steam_discover: true,
            resume_downloads: true,
            galaxy_service: true,
            galaxy_upload: false,
            raven: false,
            steam_pending: false,
            restarting_steam: false,
            steam_needs_close_confirmation: false,
            restart_error: None,
            checking_release: false,
            release: None,
            availability: self_update::availability(),
            install: None,
            modio: cx.new(ModioKeySection::new),
            nexus: cx.new(NexusSection::new),
            credentials: cx.new(CredentialsSection::new),
            debug: cx.new(DebugSection::new),
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
        self.steam_compat = crate::core::db()
            .and_then(|db| db.steam_compat_enabled().map_err(crate::core::StoreError::Query))
            .inspect_err(
                |err| tracing::warn!(error = %err, "failed to read steam_compat_enabled setting, defaulting to true"),
            )
            .unwrap_or(true);
        self.steam_register_folders = crate::core::db()
            .and_then(|db| {
                db.steam_register_folders_enabled()
                    .map_err(crate::core::StoreError::Query)
            })
            .inspect_err(
                |err| tracing::warn!(error = %err, "failed to read steam_register_folders setting, defaulting to true"),
            )
            .unwrap_or(true);
        self.steam_discover = crate::core::db()
            .and_then(|db| db.steam_discover_enabled().map_err(crate::core::StoreError::Query))
            .inspect_err(
                |err| tracing::warn!(error = %err, "failed to read steam_discover setting, defaulting to true"),
            )
            .unwrap_or(true);
        self.steam_export_enabled = crate::core::db()
            .and_then(|db| db.steam_export_enabled().map_err(crate::core::StoreError::Query))
            .inspect_err(
                |err| tracing::warn!(error = %err, "failed to read steam_export_enabled setting, defaulting to true"),
            )
            .unwrap_or(true);
        self.resume_downloads = crate::core::db()
            .and_then(|db| db.resume_downloads_on_start().map_err(crate::core::StoreError::Query))
            .inspect_err(|err| {
                tracing::warn!(error = %err, "failed to read resume_downloads_on_start setting, defaulting to true");
            })
            .unwrap_or(true);
        self.galaxy_service = crate::core::db()
            .and_then(|db| db.galaxy_service_enabled().map_err(crate::core::StoreError::Query))
            .inspect_err(|err| {
                tracing::warn!(error = %err, "failed to read galaxy_service_enabled setting, defaulting to true");
            })
            .unwrap_or(true);
        self.galaxy_upload = crate::core::db()
            .and_then(|db| db.galaxy_upload_enabled().map_err(crate::core::StoreError::Query))
            .inspect_err(|err| {
                tracing::warn!(error = %err, "failed to read galaxy_upload_enabled setting, defaulting to false");
            })
            .unwrap_or(false);
        self.steam_pending = crate::core::steam_shortcuts_pending();
        self.raven = crate::core::db()
            .and_then(|db| db.raven_enabled().map_err(crate::core::StoreError::Query))
            .inspect_err(
                |err| tracing::warn!(error = %err, "failed to read raven_enabled setting, defaulting to false"),
            )
            .unwrap_or(false);
    }

    /// Shows or hides the Raven section; nothing is downloaded until the section's own Construir.
    fn set_raven(&mut self, enabled: bool, cx: &mut Context<Self>) {
        let saved =
            crate::core::db().and_then(|db| db.set_raven_enabled(enabled).map_err(crate::core::StoreError::Query));
        if let Err(err) = saved {
            tracing::warn!(enabled, error = %err, "failed to save raven_enabled setting");
            return;
        }
        tracing::info!(enabled, "Raven section switched");
        let session = AppState::get(cx).session.clone();
        session.update(cx, |session, cx| {
            session.raven = enabled;
            cx.notify();
        });
        self.reload();
        cx.notify();
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

    fn set_steam_compat(&mut self, enabled: bool, cx: &mut Context<Self>) {
        let saved = crate::core::db().and_then(|db| {
            db.set_steam_compat_enabled(enabled)
                .map_err(crate::core::StoreError::Query)
        });
        if let Err(err) = saved {
            tracing::warn!(enabled, error = %err, "failed to save steam_compat_enabled setting");
            return;
        }
        tracing::info!(enabled, "steam compatibility setting changed");
        self.steam_compat_changed(false, cx);
    }

    fn set_steam_register_folders(&mut self, enabled: bool, cx: &mut Context<Self>) {
        let saved = crate::core::db().and_then(|db| {
            db.set_steam_register_folders_enabled(enabled)
                .map_err(crate::core::StoreError::Query)
        });
        if let Err(err) = saved {
            tracing::warn!(enabled, error = %err, "failed to save steam_register_folders setting");
            return;
        }
        tracing::info!(enabled, "steam library folder listing setting changed");
        self.steam_compat_changed(false, cx);
    }

    fn set_steam_discover(&mut self, enabled: bool, cx: &mut Context<Self>) {
        let saved = crate::core::db().and_then(|db| {
            db.set_steam_discover_enabled(enabled)
                .map_err(crate::core::StoreError::Query)
        });
        if let Err(err) = saved {
            tracing::warn!(enabled, error = %err, "failed to save steam_discover setting");
            return;
        }
        tracing::info!(enabled, "steam discovery setting changed");
        self.steam_compat_changed(!enabled, cx);
    }

    /// Applies the switches now rather than at the next start, off the UI thread, then shows
    /// whether that left a change waiting for Steam to restart. With `is_reset` the active profile's
    /// Steam games and the library folders found in Steam's list are taken out first.
    fn steam_compat_changed(&mut self, is_reset: bool, cx: &mut Context<Self>) {
        self.reload();
        cx.notify();
        let state = AppState::get(cx).clone();
        let profile_id = state.session.read(cx).active_profile.clone();
        let work = cx.background_spawn(async move {
            if is_reset && let Some(profile_id) = profile_id.as_deref() {
                crate::steam_compat::reset(profile_id);
            }
            let is_changed = crate::steam_compat::sync(profile_id.as_deref());
            is_changed || is_reset
        });
        cx.spawn(async move |this, cx| {
            let is_changed = work.await;
            if is_changed {
                state
                    .library
                    .update(cx, |library, cx| library.refresh_all_install_state(cx));
            }
            let _ = this.update(cx, |this, cx| {
                this.reload();
                cx.notify();
            });
        })
        .detach();
    }

    fn set_resume_downloads(&mut self, enabled: bool, cx: &mut Context<Self>) {
        let saved = crate::core::db().and_then(|db| {
            db.set_resume_downloads_on_start(enabled)
                .map_err(crate::core::StoreError::Query)
        });
        if let Err(err) = saved {
            tracing::warn!(enabled, error = %err, "failed to save resume_downloads_on_start setting");
            return;
        }
        tracing::info!(enabled, "resume-downloads-on-start setting changed");
        self.reload();
        cx.notify();
    }

    fn set_galaxy_service(&mut self, enabled: bool, cx: &mut Context<Self>) {
        let saved = crate::core::db().and_then(|db| {
            db.set_galaxy_service_enabled(enabled)
                .map_err(crate::core::StoreError::Query)
        });
        if let Err(err) = saved {
            tracing::warn!(enabled, error = %err, "failed to save galaxy_service_enabled setting");
            return;
        }
        tracing::info!(enabled, "local Galaxy service setting changed");
        self.reload();
        cx.notify();
    }

    fn set_galaxy_upload(&mut self, enabled: bool, cx: &mut Context<Self>) {
        let saved = crate::core::db().and_then(|db| {
            db.set_galaxy_upload_enabled(enabled)
                .map_err(crate::core::StoreError::Query)
        });
        if let Err(err) = saved {
            tracing::warn!(enabled, error = %err, "failed to save galaxy_upload_enabled setting");
            return;
        }
        tracing::info!(enabled, "Galaxy upload setting changed");
        self.reload();
        cx.notify();
    }

    fn restart_steam(&mut self, user_confirmed_closed: bool, cx: &mut Context<Self>) {
        self.restarting_steam = true;
        self.restart_error = None;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let outcome = crate::core::restart_steam_to_apply_pending(user_confirmed_closed).await;
            let _ = this.update(cx, |this, cx| {
                this.steam_needs_close_confirmation = false;
                match outcome {
                    Ok(()) => {
                        tracing::info!("steam restarted to apply pending shortcuts");
                        this.reload();
                    }
                    Err(crate::core::RestartSteamError::NeedsCloseConfirmation) => {
                        tracing::warn!("cannot tell whether steam is running, asking the user to close it");
                        this.steam_needs_close_confirmation = true;
                    }
                    Err(crate::core::RestartSteamError::Failed(err)) => {
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
        let (appimage, original) = match &self.availability {
            Availability::AppImage(appimage) => (appimage, None),
            Availability::Managed { managed, original } => (managed, original.as_deref()),
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
            let installed = if original.is_some() {
                t!("settings.general.updates.installed_managed", tag = tag)
            } else {
                t!("settings.general.updates.installed_update", tag = tag)
            };
            return column().child(div().child(installed)).child(inline(
                button(
                    "restart-decanter",
                    ButtonKind::Primary,
                    t!("settings.general.updates.restart_now"),
                    false,
                )
                .on_click(move |_, _, cx| {
                    self_update::restart_after_quit(appimage.clone());
                    crate::views::library::unfinished::quit(cx);
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
        let managed = original
            .and_then(Path::parent)
            .map(|dir| theme::muted(t!("settings.general.updates.managed", dir = dir.display().to_string())));
        column()
            .children(managed)
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
        // Where this build can't install a release itself, its page is where to get it.
        let page_label = if matches!(
            self.availability,
            Availability::AppImage(_) | Availability::Managed { .. }
        ) {
            t!("settings.general.updates.notes")
        } else {
            t!("settings.general.updates.download_page")
        };
        let newer = match &self.release {
            Some(Ok(Standing::Newer(release))) => Some(release),
            None | Some(Ok(Standing::UpToDate(_) | Standing::Ahead(_) | Standing::NoRelease) | Err(_)) => None,
        };
        let (text, page) = match &self.release {
            Some(Ok(Standing::UpToDate(release))) => {
                (Some(t!("settings.general.updates.up_to_date", tag = release.tag)), None)
            }
            Some(Ok(Standing::Ahead(release))) => (Some(t!("settings.general.updates.ahead", tag = release.tag)), None),
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
                        button("open-release-notes", ButtonKind::Secondary, page_label, false).on_click(
                            move |_, _, _| {
                                if let Err(err) = crate::desktop::open(&url) {
                                    tracing::warn!(url, %err, "could not open the release page");
                                }
                            },
                        )
                    })),
            )
            .children(text.map(theme::muted))
            .children(newer.map(|release| self.install_section(release, cx)))
            .children(error.map(theme::error))
    }

    fn checkbox_row(
        id: &'static str,
        checked: bool,
        text: impl Into<SharedString>,
        on_toggle: impl Fn(&mut Self, bool, &mut Context<Self>) + 'static,
        cx: &mut Context<Self>,
    ) -> SettingRow {
        SettingRow::new(id, checked, text).on_click(cx.listener(move |this, _, _, cx| on_toggle(this, !checked, cx)))
    }

    fn steam_section(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let is_offered = self.steam_compat;
        let sub_row =
            |id, checked, text, help, on_toggle: fn(&mut Self, bool, &mut Context<Self>), cx: &mut Context<Self>| {
                Self::checkbox_row(id, checked, text, on_toggle, cx)
                    .nested(is_offered)
                    .help(help)
            };
        column()
            .child(
                Self::checkbox_row(
                    "steam-compat",
                    self.steam_compat,
                    t!("settings.general.steam.compat"),
                    Self::set_steam_compat,
                    cx,
                )
                .help(t!("settings.general.steam.compat_hint")),
            )
            .child(sub_row(
                "steam-register-folders",
                self.steam_register_folders,
                t!("settings.general.steam.register"),
                t!("settings.general.steam.register_hint"),
                Self::set_steam_register_folders,
                cx,
            ))
            .child(sub_row(
                "steam-export",
                self.steam_export_enabled,
                t!("settings.general.steam.add"),
                t!("settings.general.steam.restart_hint"),
                Self::set_steam_export,
                cx,
            ))
            .child(sub_row(
                "steam-discover",
                self.steam_discover,
                t!("settings.general.steam.discover"),
                t!("settings.general.steam.discover_hint"),
                Self::set_steam_discover,
                cx,
            ))
    }

    fn downloads_section(&self, cx: &mut Context<Self>) -> impl IntoElement {
        column().child(
            Self::checkbox_row(
                "resume-downloads",
                self.resume_downloads,
                t!("settings.general.downloads.resume_on_start"),
                Self::set_resume_downloads,
                cx,
            )
            .help(t!("settings.general.downloads.resume_hint")),
        )
    }

    fn galaxy_section(&self, cx: &mut Context<Self>) -> impl IntoElement {
        column()
            .child(
                Self::checkbox_row(
                    "galaxy-service",
                    self.galaxy_service,
                    t!("settings.general.galaxy.service"),
                    Self::set_galaxy_service,
                    cx,
                )
                .help(t!("settings.general.galaxy.service_hint")),
            )
            .children((GALAXY_UPLOAD_AVAILABLE).then(|| {
                Self::checkbox_row(
                    "galaxy-upload",
                    self.galaxy_upload,
                    t!("settings.general.galaxy.upload"),
                    Self::set_galaxy_upload,
                    cx,
                )
                .help(t!("settings.general.galaxy.upload_hint"))
            }))
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
        let confirming = self.steam_needs_close_confirmation;
        let callout = self.steam_pending.then(|| {
            column()
                .child(theme::muted(t!("settings.general.steam.pending")))
                .when(confirming, |c| {
                    c.child(div().child(t!("settings.general.steam.close_first")))
                })
                .child(inline(
                    button(
                        "restart-steam",
                        ButtonKind::Primary,
                        if restarting {
                            t!("settings.general.steam.restarting")
                        } else if confirming {
                            t!("settings.general.steam.apply_closed")
                        } else {
                            t!("settings.general.steam.restart_now")
                        },
                        restarting,
                    )
                    .when(!restarting, |b| {
                        b.on_click(cx.listener(move |this, _, _, cx| this.restart_steam(confirming, cx)))
                    }),
                ))
                .children(self.restart_error.as_ref().map(theme::error))
        });

        match self.tab {
            Tab::General => column()
                .child(theme::h2(t!("settings.general.language.title")))
                .child(section("language", self.language_select(cx)))
                .children(language_note(self.language).map(|note| note.mb(px(12.)).max_w(px(520.))))
                .child(theme::h2(t!("settings.general.close.title")))
                .child(section(
                    "close-to-tray-section",
                    offered(
                        Feature::CloseToTray,
                        column().child(
                            Self::checkbox_row(
                                "close-to-tray",
                                self.close_to_tray,
                                t!("settings.general.close.to_tray"),
                                Self::set_close_to_tray,
                                cx,
                            )
                            .help(t!("settings.general.close.restart_hint")),
                        ),
                        cx,
                    ),
                ))
                .child(theme::h2(t!("settings.general.close_game.title")))
                .child(section(
                    "close-game-section",
                    offered(
                        Feature::PadClose,
                        column().child(
                            Self::checkbox_row(
                                "close-game-shortcut",
                                self.close_game_shortcut,
                                t!(platform::CLOSE_GAME_LABEL),
                                Self::set_close_game_shortcut,
                                cx,
                            )
                            .help(t!(
                                platform::CLOSE_GAME_HINT,
                                hold = crate::closing::HOLD.as_secs(),
                                grace = crate::closing::GRACE.as_secs()
                            )),
                        ),
                        cx,
                    ),
                )),
            Tab::Downloads => column()
                .child(theme::h2(t!("settings.general.downloads.title")))
                .child(section("downloads", self.downloads_section(cx)))
                .child(theme::h2(t!("settings.general.updates.title")))
                .child(section("updates", self.release_section(cx))),
            Tab::Integrations => column()
                .child(theme::h2(t!("settings.general.steam.title")))
                .child(section(
                    "steam-section",
                    offered(
                        Feature::SteamExport,
                        column().child(self.steam_section(cx)).children(callout),
                        cx,
                    ),
                ))
                .child(theme::h2(t!("settings.general.galaxy.title")))
                .child(section("galaxy", self.galaxy_section(cx)))
                .child(section("modio-key", self.modio.clone()))
                .child(section("nexus", self.nexus.clone())),
            Tab::Advanced => column()
                .child(theme::h2(t!("settings.general.raven.title")))
                .child(section(
                    "raven-section-block",
                    offered(
                        Feature::Raven,
                        column().child(
                            Self::checkbox_row(
                                "raven-section",
                                self.raven,
                                t!("settings.general.raven.enable"),
                                Self::set_raven,
                                cx,
                            )
                            .help(t!("settings.general.raven.hint")),
                        ),
                        cx,
                    ),
                ))
                .child(section("credentials", self.credentials.clone()))
                .child(section("debug", self.debug.clone())),
            Tab::Account | Tab::Compatibility => column(),
        }
    }
}

/// A section's controls, or why they aren't there. Its arguments are built
/// before it borrows `cx`, which the controls' listeners need too.
fn offered(feature: Feature, controls: impl IntoElement, cx: &App) -> AnyElement {
    Caps::get(cx).gate_or_reason(feature, controls)
}
