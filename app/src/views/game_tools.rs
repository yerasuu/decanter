//! One game's own choice of [`GameTool`]s — follow the library's default,
//! on, or off — shown in a PC game's Compatibilidad and a PS game's Resumen.

use decanter_core::GameTool;
use gpui::{Context, IntoElement, SharedString, Window, div, prelude::*, px};
use rust_i18n::t;

use crate::theme;
use crate::widgets::Select;

pub struct GameToolsPanel {
    game_key: String,
    mangohud: Choice,
    mangohud_installed: bool,
    error: Option<String>,
}

/// What a game has for one tool: its override, and the default it follows
/// without one.
#[derive(Default)]
struct Choice {
    chosen: Option<bool>,
    default: bool,
}

impl GameToolsPanel {
    pub fn new(game_key: String) -> Self {
        let mut this = Self {
            game_key,
            mangohud: Choice::default(),
            mangohud_installed: decanter_core::mangohud_installed(),
            error: None,
        };
        this.reload();
        this
    }

    fn reload(&mut self) {
        let read = crate::core::db().map_err(|err| err.localized()).and_then(|db| {
            let chosen = db.game_tool_override(&self.game_key, GameTool::MangoHud);
            let default = db.game_tool_default(GameTool::MangoHud);
            chosen
                .and_then(|chosen| default.map(|default| Choice { chosen, default }))
                .map_err(|err| err.to_string())
        });
        match read {
            Ok(choice) => {
                self.mangohud = choice;
                self.error = None;
            }
            Err(err) => {
                tracing::warn!(game_key = %self.game_key, error = %err, "failed to read the game's tool choices");
                self.error = Some(err);
            }
        }
    }

    fn set(&mut self, tool: GameTool, chosen: &str, cx: &mut Context<Self>) {
        let chosen = match chosen {
            "1" => Some(true),
            "0" => Some(false),
            _ => None,
        };
        let saved = crate::core::db().map_err(|err| err.localized()).and_then(|db| {
            db.set_game_tool_override(&self.game_key, tool, chosen)
                .map_err(|err| err.to_string())
        });
        match saved {
            Ok(()) => self.reload(),
            Err(err) => {
                tracing::warn!(game_key = %self.game_key, tool = tool.key(), error = %err, "failed to save the game's tool choice");
                self.error = Some(err);
            }
        }
        cx.notify();
    }

    fn select(id: &'static str, tool: GameTool, choice: &Choice, cx: &Context<Self>) -> Select {
        let default = if choice.default {
            t!("game_tools.default_on")
        } else {
            t!("game_tools.default_off")
        };
        let options = [
            (SharedString::default(), SharedString::from(default)),
            ("1".into(), t!("game_tools.on").into()),
            ("0".into(), t!("game_tools.off").into()),
        ];
        let selected = match choice.chosen {
            Some(true) => "1",
            Some(false) => "0",
            None => "",
        };
        let entity = cx.entity();
        Select::new(id, options, selected).on_change(move |chosen, _, cx| {
            entity.update(cx, |this, cx| this.set(tool, chosen, cx));
        })
    }
}

/// A tool's name on the left, its select or why there is none on the right.
fn line(name: &'static str, control: impl IntoElement) -> gpui::Div {
    div()
        .flex()
        .items_center()
        .gap_4()
        .mb_3()
        .child(div().w(px(110.)).flex_none().child(name))
        .child(control)
}

impl Render for GameToolsPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let column = div()
            .flex()
            .flex_col()
            .child(theme::h2(t!("game_tools.title")))
            .child(theme::muted(t!("game_tools.intro")).mb_3());
        if !cfg!(target_os = "linux") {
            return column.child(theme::muted(t!("common.unavailable_here")));
        }
        let mangohud = if self.mangohud_installed {
            Self::select("tool-mangohud", GameTool::MangoHud, &self.mangohud, cx).into_any_element()
        } else {
            theme::muted(t!("game_tools.mangohud_missing")).into_any_element()
        };
        column
            .child(line("MangoHud", mangohud))
            .children(self.error.clone().map(theme::error))
    }
}
