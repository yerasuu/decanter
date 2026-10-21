//! Which Proton this game runs on, and the escape hatch when its prefix
//! goes bad — Steam's own Properties → Compatibilidad, moved out of a
//! library-wide screen since both are already scoped to one game.

use std::rc::Rc;

use decanter_core::InstalledGame;
use decanter_core::capabilities::Feature;
use gpui::{Context, Entity, SharedString, Window, prelude::*};
use rust_i18n::t;

use super::{Target, error_line, field_row, not_installed, pane, row, rows};
use crate::capabilities::Caps;
use crate::i18n::Text;
use crate::theme;
use crate::views::game_tools::GameToolsPanel;
use crate::widgets::{ButtonKind, Select, button, tooltip};

pub(super) struct CompatibilityTab {
    target: Rc<Target>,
    installed: Option<InstalledGame>,
    runners: Vec<String>,
    created: bool,
    busy: bool,
    error: Option<Text>,
    tools: Entity<GameToolsPanel>,
}

impl CompatibilityTab {
    pub(super) fn new(target: Rc<Target>, cx: &mut Context<Self>) -> Self {
        let key = crate::taskbar::key(target.storefront, &target.product_id);
        let mut this = Self {
            tools: cx.new(|_| GameToolsPanel::new(key)),
            target,
            installed: None,
            runners: Vec::new(),
            created: false,
            busy: false,
            error: None,
        };
        this.reload();
        this
    }

    fn reload(&mut self) {
        let account_id = self.target.account_id.as_str();
        let product_id = self.target.product_id.as_str();
        self.installed = crate::core::installed_game(account_id, product_id);
        self.runners = decanter_runtime::steam_runners();
        // A prefix that was never launched into doesn't exist yet, which is
        // the normal state for a game you installed but haven't opened.
        self.created = crate::core::prefix_for(account_id, product_id).is_some_and(|prefix| prefix.path().is_dir());
    }

    fn pin(&mut self, chosen: &str, cx: &mut Context<Self>) {
        let chosen = (!chosen.is_empty()).then_some(chosen);
        match set_proton(&self.target.account_id, &self.target.product_id, chosen) {
            Ok(()) => {
                self.error = None;
                self.reload();
            }
            Err(err) => self.error = Some(err),
        }
        cx.notify();
    }

    fn reset_prefix(&mut self, cx: &mut Context<Self>) {
        let account_id = self.target.account_id.clone();
        let game_id = self.target.product_id.clone();
        self.busy = true;
        tracing::info!(account_id, product_id = %game_id, "resetting proton prefix");
        cx.notify();
        cx.spawn(async move |this, cx| {
            let outcome = crate::launch::reset_prefix(account_id.clone(), game_id.clone()).await;
            let _ = this.update(cx, |this, cx| {
                match outcome {
                    Ok(()) => {
                        tracing::info!(account_id, product_id = %game_id, "proton prefix reset");
                        this.error = None;
                        this.reload();
                    }
                    Err(err) => {
                        tracing::error!(account_id, product_id = %game_id, error = %err, "failed to reset proton prefix");
                        this.error = Some(err);
                    }
                }
                this.busy = false;
                cx.notify();
            });
        })
        .detach();
    }
}

impl Render for CompatibilityTab {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(installed) = &self.installed else {
            return not_installed();
        };
        if let Some(reason) = Caps::get(cx).reason(Feature::Proton) {
            return pane().child(theme::muted(reason));
        }

        let options = std::iter::once((
            SharedString::default(),
            SharedString::from(t!("library.properties.compatibility.default_proton")),
        ))
        .chain(self.runners.iter().map(|runner| {
            let runner = SharedString::from(runner.clone());
            (runner.clone(), runner)
        }));
        let entity = cx.entity();
        // A pin to a Proton that's no longer installed shows as the default,
        // the way a native select falls back to its first option.
        let selected = installed
            .proton
            .clone()
            .filter(|pinned| self.runners.contains(pinned))
            .unwrap_or_default();
        let proton = Select::new("proton", options, selected).on_change(move |chosen, _, cx| {
            entity.update(cx, |this, cx| this.pin(chosen, cx));
        });

        let reset_disabled = !self.created || self.busy;
        let reset = button(
            "reset-prefix",
            ButtonKind::Secondary,
            if self.busy {
                t!("library.properties.compatibility.resetting")
            } else {
                t!("library.properties.compatibility.reset")
            },
            reset_disabled,
        )
        .tooltip(tooltip(t!("library.properties.compatibility.reset_tooltip")))
        .when(!reset_disabled, |b| {
            b.on_click(cx.listener(|this, _, _, cx| this.reset_prefix(cx)))
        });

        pane()
            .child(rows().child(row(
                t!("library.properties.compatibility.prefix"),
                if self.created {
                    t!("library.properties.compatibility.prefix_created")
                } else {
                    t!("library.properties.compatibility.prefix_missing")
                },
            )))
            .child(field_row().child(proton).child(reset))
            .children(error_line(self.error.as_ref()))
            .child(self.tools.clone())
    }
}

/// Pins (or unpins) a game's Proton by rewriting its manifest — the
/// manifests are the record, so there's nowhere else to put it.
fn set_proton(account_id: &str, product_id: &str, proton: Option<&str>) -> Result<(), Text> {
    let library = crate::achv_try!(crate::core::library_for_account(account_id).inspect_err(
        |err| tracing::error!(account_id, product_id, error = %err, "failed to resolve library, proton pin not changed")
    ));
    // Re-read rather than editing the copy this tab rendered from, so a
    // change made elsewhere since isn't clobbered.
    let mut current = crate::achv_try!(
        crate::core::installed_game(account_id, product_id).ok_or_else(|| Text::new("messages.errors.not_installed"))
    );
    current.proton = proton.map(str::to_string);
    match library.write_manifest(&current) {
        Ok(()) => {
            tracing::info!(account_id, product_id, proton, "proton pin changed");
            Ok(())
        }
        Err(err) => {
            tracing::error!(account_id, product_id, proton, error = %err, "failed to save proton pin");
            Err(Text::new("messages.errors.save_manifest").arg("error", err))
        }
    }
}
