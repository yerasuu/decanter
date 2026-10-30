//! Staged edits, as Steam Input keeps them: on the drawing, rebinds and restores pile up in a draft
//! of the game's lines and reach the game's port ini only on Aplicar, and Cancelar drops them. The
//! draft lives as long as the view, so leaving the tab keeps it. Applied edits are the game's own,
//! on whichever configuration it follows, a template included; none is written into the catalog.

use decanter_ps::bbport::pad::{MENU, PadLayout};
use gpui::Context;

use super::GamepadView;

/// The tabs of the drawing: what the game does with each input, and what
/// the port itself takes from the pad.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Scope {
    Game,
    Port,
}

impl Scope {
    pub(super) const ALL: [Self; 2] = [Self::Game, Self::Port];

    pub(super) fn owns(self, input: &str) -> bool {
        (input == MENU.0) == (self == Self::Port)
    }

    pub(super) const fn label(self) -> &'static str {
        match self {
            Self::Game => "ps.gamepad.scope.game",
            Self::Port => "ps.gamepad.scope.port",
        }
    }
}

impl GamepadView {
    /// The game's lines as written, under the configuration it follows (its name, its template),
    /// or alone when it follows none.
    pub(super) fn saved(&self) -> Option<PadLayout> {
        let loaded = self.loaded()?;
        Some(loaded.layout().map_or_else(
            || loaded.game.clone(),
            |layout| PadLayout {
                lines: loaded.game.lines.clone(),
                ..layout
            },
        ))
    }

    /// What the drawing shows: the draft while there is one.
    pub(super) fn edited(&self) -> Option<PadLayout> {
        self.draft.clone().or_else(|| self.saved())
    }

    /// Keeps `draft` only while it differs from what is written, so undoing
    /// every change by hand leaves nothing to apply.
    fn keep(&mut self, draft: PadLayout, cx: &mut Context<Self>) {
        self.draft = self.saved().filter(|saved| saved.lines != draft.lines).map(|_| draft);
        cx.notify();
    }

    pub(super) fn stage(&mut self, input: &str, button: &str, cx: &mut Context<Self>) {
        if let Some(mut draft) = self.edited() {
            draft.assign(input, button);
            self.keep(draft, cx);
        }
    }

    /// Puts the open tab's inputs back on the template's buttons; the
    /// other tab keeps its edits.
    pub(super) fn restore(&mut self, cx: &mut Context<Self>) {
        let scope = self.scope;
        if let Some(mut draft) = self.edited() {
            draft.restore(|input| scope.owns(input));
            self.keep(draft, cx);
        }
    }

    pub(super) fn cancel(&mut self, cx: &mut Context<Self>) {
        self.draft = None;
        cx.notify();
    }

    /// Writes the draft into the game's port ini, as its own change.
    pub(super) fn commit(&mut self, cx: &mut Context<Self>) {
        if let Some(draft) = self.draft.take() {
            self.write_game(draft, None, cx);
        }
    }
}
