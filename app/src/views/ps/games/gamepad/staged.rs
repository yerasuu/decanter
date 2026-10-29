//! Staged edits, as Steam Input keeps them: on the drawing, rebinds and
//! restores pile up in a draft of the configuration in use and reach
//! the game's port ini only on Aplicar, and Cancelar drops them. The draft lives
//! as long as the view, so leaving the tab keeps it. A template takes edits
//! too: since templates are never written, applying them makes a new
//! configuration of the user's own.

use decanter_ps::bbport::pad::{MENU, PadLayout};
use gpui::Context;
use rust_i18n::t;

use super::{GamepadView, layout_name};

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
    /// The configuration in use as written, or `bbport.ini`'s own lines
    /// when they match none.
    pub(super) fn saved(&self) -> Option<PadLayout> {
        let loaded = self.loaded()?;
        Some(loaded.layout().unwrap_or_else(|| loaded.custom.clone()))
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

    /// Writes the draft: into the user's configuration in use, or into a
    /// new one copied from the template (or hand-edited lines) it started
    /// from.
    pub(super) fn commit(&mut self, cx: &mut Context<Self>) {
        let Some(draft) = self.draft.take() else {
            return;
        };
        let Some(loaded) = self.loaded_mut() else {
            return;
        };
        let own = loaded.layout().filter(|layout| !layout.builtin);
        let layout = if let Some(mut own) = own {
            own.lines = draft.lines;
            loaded.store.update_pad(&own);
            own
        } else {
            let name = t!(
                "ps.layouts.copy_name",
                name = layout_name(&draft.id, draft.builtin, &draft.name)
            );
            loaded.store.copy_pad(&draft, name.into_owned())
        };
        self.apply(layout, true, cx);
    }
}
