//! What the main window shows around a file drop: the highlight while files
//! are dragged over it, and the notice saying what the drop added, with
//! «Editar» and «Deshacer».

use gpui::{App, Div, FontWeight, SharedString, Stateful, div, prelude::*, px, rgb};
use rust_i18n::t;

use crate::external::dropped::Outcome;
use crate::i18n::Text;
use crate::state::AppState;
use crate::theme;
use crate::widgets::{ButtonKind, button, tooltip};

/// One drop's notice, kept until it is closed, undone or replaced by the
/// next drop.
pub struct DropNotice {
    pub profile_id: Option<String>,
    pub outcome: Outcome,
}

impl DropNotice {
    pub fn no_profile() -> Self {
        Self {
            profile_id: None,
            outcome: Outcome::default(),
        }
    }

    /// The ids «Deshacer» removes.
    pub fn added(&self) -> Vec<String> {
        self.outcome.added.iter().map(|game| game.id.clone()).collect()
    }

    /// The one game «Editar» opens; with several there is no single form
    /// to open, so the list in Juegos externos is where to edit them.
    pub fn editable(&self) -> Option<&str> {
        match self.outcome.added.as_slice() {
            [only] => Some(only.id.as_str()),
            _ => None,
        }
    }

    fn lines(&self) -> Vec<Text> {
        if self.profile_id.is_none() {
            return vec![Text::new("external.drop.no_profile")];
        }
        let outcome = &self.outcome;
        let mut lines = Vec::new();
        match outcome.added.as_slice() {
            [] => {}
            [only] => lines.push(Text::new("external.drop.added.one").arg("name", &only.name)),
            several => lines.push(
                Text::new("external.drop.added.other").arg("count", several.len()).arg(
                    "names",
                    several
                        .iter()
                        .map(|game| game.name.as_str())
                        .collect::<Vec<_>>()
                        .join(", "),
                ),
            ),
        }
        lines.extend(
            outcome
                .existing
                .iter()
                .map(|game| Text::new("external.drop.existing").arg("name", &game.name)),
        );
        lines.extend(outcome.refused.iter().map(|refused| {
            Text::new("external.drop.refused.line")
                .arg("file", &refused.file)
                .text("reason", refused.reason.clone())
        }));
        lines
    }
}

/// Over the whole window while files are dragged across it. It has no
/// listeners, so it never takes the drop from the window's own handler.
pub fn highlight() -> Div {
    div()
        .absolute()
        .inset_0()
        .flex()
        .items_center()
        .justify_center()
        .bg(theme::alpha(theme::BG_DEEP, 0.7))
        .border_2()
        .border_color(rgb(theme::ACCENT_BLUE))
        .child(
            div()
                .px_6()
                .py_4()
                .rounded(px(theme::RADIUS_LG))
                .bg(rgb(theme::BG_PANEL))
                .text_color(rgb(theme::TEXT_BRIGHT))
                .font_weight(FontWeight::SEMIBOLD)
                .child(t!("external.drop.hint")),
        )
}

/// The notice in the window's corner. `on_edit`, `on_undo` and `on_close`
/// are the shell's, which owns the notice.
pub fn render(
    notice: &DropNotice,
    on_edit: impl Fn(&mut App) + 'static,
    on_undo: impl Fn(&mut App) + 'static,
    on_close: impl Fn(&mut App) + 'static,
    cx: &App,
) -> Stateful<Div> {
    let added = notice.added();
    // Undoing deletes the prefix, which a running game is still using.
    let running = !added.is_empty() && {
        let running = &AppState::get(cx).background.read(cx).running;
        added.iter().any(|id| running.contains(id))
    };
    let actions = div()
        .flex()
        .justify_end()
        .gap_2()
        .mt_1()
        .when(notice.editable().is_some(), |row| {
            row.child(
                button("drop-edit", ButtonKind::Secondary, t!("external.drop.edit"), false)
                    .on_click(move |_, _, cx| on_edit(cx)),
            )
        })
        .when(!added.is_empty(), |row| {
            row.child(
                button("drop-undo", ButtonKind::Secondary, t!("external.drop.undo"), running)
                    .when(running, |b| b.tooltip(tooltip(t!("external.drop.undo_running"))))
                    .when(!running, |b| b.on_click(move |_, _, cx| on_undo(cx))),
            )
        })
        .child(
            button("drop-close", ButtonKind::Icon, t!("external.drop.close"), false)
                .on_click(move |_, _, cx| on_close(cx)),
        );
    div()
        .id("drop-notice")
        .absolute()
        .right(px(18.))
        .bottom(px(18.))
        .max_w(px(460.))
        .flex()
        .flex_col()
        .gap_1()
        .p_4()
        .rounded(px(theme::RADIUS))
        .bg(rgb(theme::BG_PANEL))
        .border_1()
        .border_color(rgb(theme::LINE))
        .shadow(theme::popup_shadow())
        .text_size(px(12.5))
        .text_color(rgb(theme::TEXT_PRIMARY))
        .occlude()
        .children(
            notice
                .lines()
                .into_iter()
                .map(|line| div().child(SharedString::from(line))),
        )
        .child(actions)
}
