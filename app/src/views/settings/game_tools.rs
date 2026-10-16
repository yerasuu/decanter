//! The library-wide default of each [`GameTool`], for every game without a
//! choice of its own (set in its Propiedades, or a PS game's Resumen).

use decanter_core::GameTool;
use gpui::{Context, IntoElement, SharedString, Window, div, prelude::*};
use rust_i18n::t;

use super::{column, field_row};
use crate::theme;
use crate::widgets::checkbox;

pub struct GameToolDefaults {
    mangohud_installed: bool,
    mangohud: bool,
    mangohud_autostart_log: bool,
    gamemode_installed: bool,
    gamemode: bool,
    error: Option<String>,
}

impl GameToolDefaults {
    pub fn new() -> Self {
        let mut this = Self {
            mangohud_installed: decanter_core::mangohud_installed(),
            mangohud: false,
            mangohud_autostart_log: false,
            gamemode_installed: decanter_core::gamemoded().is_some(),
            gamemode: false,
            error: None,
        };
        this.reload();
        this
    }

    fn reload(&mut self) {
        let read = crate::core::db().map_err(|err| err.localized()).and_then(|db| {
            let read = || -> decanter_core::Result<_> {
                Ok((
                    db.game_tool_default(GameTool::MangoHud)?,
                    db.mangohud_autostart_log()?,
                    db.game_tool_default(GameTool::GameMode)?,
                ))
            };
            read().map_err(|err| err.to_string())
        });
        match read {
            Ok((mangohud, log, gamemode)) => {
                self.mangohud = mangohud;
                self.mangohud_autostart_log = log;
                self.gamemode = gamemode;
                self.error = None;
            }
            Err(err) => {
                tracing::warn!(error = %err, "failed to read the game tool defaults");
                self.error = Some(err);
            }
        }
    }

    fn save(&mut self, setting: &'static str, change: impl FnOnce(&decanter_core::Db) -> decanter_core::Result<()>) {
        let saved = crate::core::db()
            .map_err(|err| err.localized())
            .and_then(|db| change(&db).map_err(|err| err.to_string()));
        match saved {
            Ok(()) => {
                tracing::info!(setting, "game tool default changed");
                self.reload();
            }
            Err(err) => {
                tracing::warn!(setting, error = %err, "failed to save the game tool default");
                self.error = Some(err);
            }
        }
    }

    fn set_mangohud(&mut self, enabled: bool, cx: &mut Context<Self>) {
        self.save("mangohud", |db| db.set_game_tool_default(GameTool::MangoHud, enabled));
        cx.notify();
    }

    fn set_gamemode(&mut self, enabled: bool, cx: &mut Context<Self>) {
        self.save("gamemode", |db| db.set_game_tool_default(GameTool::GameMode, enabled));
        cx.notify();
    }

    fn set_mangohud_autostart_log(&mut self, enabled: bool, cx: &mut Context<Self>) {
        self.save("mangohud_autostart_log", |db| db.set_mangohud_autostart_log(enabled));
        cx.notify();
    }

    /// The whole row toggles its checkbox, the way clicking a `<label>`'s text does.
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
}

impl Render for GameToolDefaults {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let section = column()
            .child(theme::h2(t!("settings.compatibility.game_tools.title")))
            .child(theme::muted(t!("settings.compatibility.game_tools.intro")).mb_3());
        let section = if self.mangohud_installed {
            section
                .child(Self::checkbox_row(
                    "default-mangohud",
                    self.mangohud,
                    t!("settings.compatibility.game_tools.mangohud"),
                    Self::set_mangohud,
                    cx,
                ))
                .child(Self::checkbox_row(
                    "mangohud-autostart-log",
                    self.mangohud_autostart_log,
                    t!("settings.compatibility.game_tools.mangohud_log"),
                    Self::set_mangohud_autostart_log,
                    cx,
                ))
                .child(theme::muted(t!("settings.compatibility.game_tools.mangohud_log_hint")))
        } else {
            section.child(theme::muted(t!("game_tools.mangohud_missing")))
        };
        let section = if self.gamemode_installed {
            section
                .child(Self::checkbox_row(
                    "default-gamemode",
                    self.gamemode,
                    t!("settings.compatibility.game_tools.gamemode"),
                    Self::set_gamemode,
                    cx,
                ))
                .child(theme::muted(t!("settings.compatibility.game_tools.gamemode_hint")))
        } else {
            section.child(theme::muted(t!("game_tools.gamemode_missing")))
        };
        section.children(self.error.clone().map(theme::error))
    }
}
