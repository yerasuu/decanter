//! The Proton every game runs on unless it pins one of its own. Per-game
//! pinning and prefix resets live in each game's own Propiedades instead
//! (Steam's own shape: that's a per-game concern, not this screen's).

use gpui::{Context, IntoElement, SharedString, Window, prelude::*};
use rust_i18n::t;

use super::{column, field_row};
use crate::theme;
use crate::widgets::Select;

pub struct CompatibilityTab {
    runners: Vec<String>,
    current: Option<String>,
}

impl CompatibilityTab {
    pub fn new(_: &mut Context<Self>) -> Self {
        let mut this = Self {
            runners: Vec::new(),
            current: None,
        };
        this.reload();
        this
    }

    fn reload(&mut self) {
        self.runners = decanter_runtime::steam_runners();
        self.current = crate::core::db()
            .inspect_err(|err| tracing::warn!(error = %err, "failed to open db, default proton unknown"))
            .ok()
            .and_then(|db| {
                db.default_proton()
                    .inspect_err(|err| tracing::warn!(error = %err, "failed to read default proton setting"))
                    .ok()
                    .flatten()
            });
    }

    fn set_default(&mut self, chosen: &str, cx: &mut Context<Self>) {
        let chosen = (!chosen.is_empty()).then_some(chosen);
        match crate::core::db() {
            Ok(db) => match db.set_default_proton(chosen) {
                Ok(()) => {
                    tracing::info!(proton = chosen, "default proton pin changed");
                    self.reload();
                    cx.notify();
                }
                Err(err) => tracing::warn!(proton = chosen, error = %err, "failed to save default proton setting"),
            },
            Err(err) => {
                tracing::warn!(proton = chosen, error = %err, "failed to open db, default proton not changed");
            }
        }
    }
}

impl Render for CompatibilityTab {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // A pin naming a Proton that's since been uninstalled matches no
        // option, and a native select falls back to its first one then —
        // so does this, rather than showing an empty trigger.
        let selected = self
            .current
            .as_deref()
            .filter(|current| self.runners.iter().any(|runner| runner == current))
            .unwrap_or_default()
            .to_owned();
        let options = std::iter::once((SharedString::default(), t!("settings.compatibility.automatic").into())).chain(
            self.runners
                .iter()
                .map(|runner| (SharedString::from(runner.clone()), SharedString::from(runner.clone()))),
        );
        let entity = cx.entity();

        column()
            .child(theme::muted(t!("settings.compatibility.intro")))
            .child(theme::h2(t!("settings.compatibility.default")))
            .child(
                field_row().child(
                    Select::new("default-proton", options, selected).on_change(move |chosen, _, cx| {
                        entity.update(cx, |this, cx| this.set_default(chosen, cx));
                    }),
                ),
            )
            .when(self.runners.is_empty(), |column| {
                column.child(theme::muted(t!("settings.compatibility.none_installed")))
            })
    }
}
