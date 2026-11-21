//! A Steam game's mods are copied into its folder rather than started from a play tree, since
//! Steam starts the game (decision #1725): each reload of the tab checks the folder and copies
//! again what a Steam verify put back, and this panel says how the folder stands, with Reapply.

use decanter_mods::{Deployment, EffectiveOverride, GameMods, Takeover};
use gpui::{Context, Div, div, prelude::*, px};
use rust_i18n::t;

use super::ModsTab;
use crate::i18n::Text;
use crate::theme;
use crate::widgets::{ButtonKind, button};

/// The last pass over the game's folder.
pub(super) enum Applied {
    Done(Deployment),
    /// Not run, since the game runs and may hold the files open.
    Waiting,
    Failed(String),
}

/// Copies the account's mods into the folder unless the game runs; `None` for a game that starts
/// from a play tree.
pub(super) fn apply(mods: &GameMods, account_id: &str, is_running: bool) -> Option<Applied> {
    if !mods.copies_into_install() {
        return None;
    }
    if is_running {
        return Some(Applied::Waiting);
    }
    Some(match mods.deploy(account_id, Takeover::Never) {
        Ok(done) => Applied::Done(done),
        Err(err) => {
            tracing::error!(error = %err, "the mods could not be copied into the steam game's folder");
            Applied::Failed(err.to_string())
        }
    })
}

/// What to add to the game's launch options in Steam for the enabled mods' DLL overrides, which
/// decanter can't pass to a game Steam starts.
fn launch_options(overrides: &[EffectiveOverride]) -> Option<String> {
    if overrides.is_empty() {
        return None;
    }
    let value = overrides
        .iter()
        .map(|wanted| format!("{}={}", wanted.dll, wanted.mode))
        .collect::<Vec<_>>()
        .join(";");
    Some(format!("WINEDLLOVERRIDES=\"{value}\" %command%"))
}

impl ModsTab {
    pub(super) fn copies_into_install(&self) -> bool {
        self.mods.as_ref().is_some_and(GameMods::copies_into_install)
    }

    /// The line a switch leaves, since a Steam game's folder changes at once and any other game's
    /// play tree at its next launch.
    pub(super) fn applies_key(&self) -> &'static str {
        if self.copies_into_install() {
            "mods.steam.applies_now"
        } else {
            "mods.toggles.applies_at_launch"
        }
    }

    pub(super) fn steam_panel(&self, locked: bool, cx: &Context<Self>) -> Option<Div> {
        let loaded = self.loaded.as_ref()?;
        let applied = loaded.applied.as_ref()?;
        let mut lines = Vec::new();
        match applied {
            Applied::Waiting => lines.push(theme::muted(t!("mods.steam.waiting"))),
            Applied::Failed(error) => lines.push(theme::error(t!("mods.steam.failed", error = error))),
            Applied::Done(done) if done.other_account.is_some() => {
                lines.push(theme::error(t!("mods.steam.other_profile", count = done.placed)));
            }
            Applied::Done(done) => {
                lines.push(if done.placed == 0 {
                    theme::muted(t!("mods.steam.none_applied"))
                } else {
                    theme::muted(t!("mods.steam.applied", count = done.placed))
                });
                if done.reapplied > 0 {
                    lines.push(theme::muted(t!("mods.steam.reapplied", count = done.reapplied)));
                }
                if done.restored > 0 {
                    lines.push(theme::muted(t!("mods.steam.restored", count = done.restored)));
                }
                if done.changed > 0 {
                    lines.push(theme::error(t!("mods.steam.changed", count = done.changed)));
                }
                if done.skipped > 0 {
                    lines.push(theme::error(t!("mods.steam.skipped", count = done.skipped)));
                }
            }
        }
        let dll = launch_options(&loaded.dll).map(|options| {
            div()
                .flex()
                .flex_col()
                .gap_1()
                .child(theme::muted(t!("mods.steam.dll")))
                .child(theme::mono().child(options))
        });
        let can_reapply = !matches!(applied, Applied::Waiting);
        Some(
            theme::card()
                .gap_2()
                .px(px(18.))
                .py(px(14.))
                .children(lines)
                .children(dll)
                .when(can_reapply, |card| {
                    card.child(
                        div().flex().child(
                            button(
                                "mods-steam-reapply",
                                ButtonKind::Secondary,
                                t!("mods.steam.reapply"),
                                locked,
                            )
                            .when(!locked, |b| b.on_click(cx.listener(|this, _, _, cx| this.reapply(cx)))),
                        ),
                    )
                }),
        )
    }

    /// Copies this account's mods in again, also over another profile's.
    fn reapply(&mut self, cx: &mut Context<Self>) {
        self.run(
            Text::new("mods.steam.applying"),
            |mods, account| mods.deploy(account, Takeover::Allowed).map(|_| ()),
            cx,
        );
    }
}
