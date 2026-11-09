//! A mod an update changed a game file under, left out of the play tree until the user keeps it
//! or disables it (design, section 3: no automatic decision, a mod built for another build can
//! crash the game either way); and the check of a mod's own files against what was imported.

use decanter_mods::ModId;
use gpui::{Context, Div, div, prelude::*, px};
use rust_i18n::t;

use super::{ModsTab, Row, path_list};
use crate::i18n::Text;
use crate::theme;
use crate::widgets::{ButtonKind, button};

impl ModsTab {
    pub(super) fn attention_block(&self, index: usize, row: &Row, locked: bool, cx: &Context<Self>) -> Option<Div> {
        let paths = row.attention.as_ref()?;
        let (keep_id, disable_id) = (row.manifest.id.clone(), row.manifest.id.clone());
        Some(
            div()
                .flex()
                .flex_col()
                .gap_1()
                .pl(px(29.))
                .text_size(px(12.5))
                .child(theme::error(t!("mods.attention.line", count = paths.len())))
                .children(path_list(paths))
                .child(
                    div()
                        .flex()
                        .gap(px(10.))
                        .child(
                            button(
                                ("mod-keep", index),
                                ButtonKind::Secondary,
                                t!("mods.attention.keep"),
                                locked,
                            )
                            .when(!locked, |b| {
                                b.on_click(cx.listener(move |this, _, _, cx| this.keep(keep_id.clone(), cx)))
                            }),
                        )
                        .child(
                            button(
                                ("mod-disable", index),
                                ButtonKind::Secondary,
                                t!("mods.attention.disable"),
                                locked,
                            )
                            .when(!locked, |b| {
                                b.on_click(
                                    cx.listener(move |this, _, _, cx| this.toggle(disable_id.clone(), false, cx)),
                                )
                            }),
                        ),
                ),
        )
    }

    pub(super) fn verify_block(&self, index: usize, row: &Row, locked: bool, cx: &Context<Self>) -> Div {
        let id = row.manifest.id.clone();
        let result = self.verified.get(&id).map(|changed| {
            if changed.is_empty() {
                div().child(theme::muted(t!("mods.verify.clean")))
            } else {
                div()
                    .flex()
                    .flex_col()
                    .child(theme::error(t!("mods.verify.changed", count = changed.len())))
                    .children(path_list(changed))
            }
        });
        div()
            .flex()
            .flex_col()
            .gap_1()
            .pl(px(29.))
            .text_size(px(12.5))
            .child(div().flex().child(
                button(("mod-verify", index), ButtonKind::Icon, t!("mods.verify.check"), locked).when(!locked, |b| {
                    b.on_click(cx.listener(move |this, _, _, cx| this.verify(id.clone(), cx)))
                }),
            ))
            .children(result)
    }

    fn keep(&mut self, id: ModId, cx: &mut Context<Self>) {
        self.run(
            Text::new("library.detail.mods.applying"),
            move |mods, account| mods.keep(account, &id),
            cx,
        );
    }

    fn verify(&mut self, id: ModId, cx: &mut Context<Self>) {
        let Some(mods) = self.mods.clone() else {
            return;
        };
        self.busy = Some(Text::new("mods.verify.checking"));
        self.error = None;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let checked = {
                let id = id.clone();
                tokio::task::spawn_blocking(move || mods.changed_files(&id)).await
            };
            let _ = this.update(cx, |this, cx| {
                this.busy = None;
                match checked {
                    Ok(Ok(changed)) => {
                        this.verified.insert(id, changed);
                    }
                    Ok(Err(err)) => {
                        tracing::error!(error = %err, "mod files not checked");
                        this.error = Some(Text::verbatim(err));
                    }
                    Err(err) => {
                        tracing::error!(error = %err, "checking the mod files stopped");
                        this.error = Some(Text::verbatim(err));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }
}
