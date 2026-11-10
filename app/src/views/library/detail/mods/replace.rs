//! A provider's newer file of a mod the game has taking that mod's slot (design, section 5.6): the
//! new mod goes where the old one was in every account's load order, on or off as it was, with its
//! DLL modes, and the old one is removed, or kept turned off right after it when the user asks in
//! the confirmation, so that going back is one toggle. Its credits are the ones the provider gave
//! with the newer file, written by the import.

use decanter_mods::{GameMods, ModId, ModManifest};
use gpui::{Context, Div, div, prelude::*, px};
use rust_i18n::t;

use super::ModsTab;
use crate::theme;
use crate::widgets::checkbox;

/// The installed mod an import replaces.
pub(super) struct Replacement {
    old: ModId,
    name: String,
    version: String,
}

impl Replacement {
    pub(super) fn of(manifest: &ModManifest) -> Self {
        Self {
            old: manifest.id.clone(),
            name: manifest.name.clone(),
            version: manifest
                .origin
                .as_ref()
                .map(|origin| origin.version.clone())
                .unwrap_or_default(),
        }
    }

    /// Puts `new`, just imported, in the old mod's slot.
    pub(super) fn apply(&self, mods: &GameMods, new: &ModId, keep_previous: bool) -> decanter_mods::Result<()> {
        tracing::info!(old = %self.old, new = %new, keep_previous, "provider update takes its mod's slot");
        if keep_previous {
            mods.replace_keeping_previous(&self.old, new)
        } else {
            mods.replace(&self.old, new)
        }
    }

    /// What the confirmation says of it, with the opt-in to keep the previous version.
    pub(super) fn block(
        &self,
        id: &'static str,
        keep_previous: bool,
        on_toggle: impl Fn(&mut ModsTab, &mut Context<ModsTab>) + 'static,
        cx: &Context<ModsTab>,
    ) -> Div {
        div()
            .flex()
            .flex_col()
            .gap_1()
            .text_size(px(12.5))
            .child(theme::muted(t!(
                "mods.replace.takes_slot",
                name = self.name,
                version = self.version
            )))
            .child(
                div()
                    .id(id)
                    .flex()
                    .items_center()
                    .gap_2()
                    .cursor_pointer()
                    .on_click(cx.listener(move |this, _, _, cx| on_toggle(this, cx)))
                    .child(checkbox(keep_previous))
                    .child(theme::muted(t!("mods.replace.keep_previous"))),
            )
    }
}
