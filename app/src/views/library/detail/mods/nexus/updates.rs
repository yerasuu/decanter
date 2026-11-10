//! *Buscar actualizaciones* on a mod that came from Nexus Mods: the file its `mod.toml` origin
//! names against the mod's file list, asked only when the user presses it, since the acceptable
//! use policy wants every request to come from a user action.

use decanter_mods::{ModId, ModOrigin};
use decanter_nexus::{NexusOrigin, Update};
use gpui::{Context, Div, div, prelude::*, px};
use rust_i18n::t;

use super::ModsTab;
use crate::i18n::Text;
use crate::nexus::failure_text;
use crate::theme;
use crate::widgets::{ButtonKind, button};

pub(in crate::views::library::detail::mods) enum UpdateCheck {
    Checking,
    UpToDate,
    /// A newer file, by its name and version.
    Newer(String),
    /// Neither the file nor any main file is listed any more.
    Gone,
    Failed(Text),
}

impl ModsTab {
    /// The origin line of a mod downloaded from Nexus Mods, with its update check.
    pub(in crate::views::library::detail::mods) fn nexus_origin_block(
        &self,
        index: usize,
        id: &ModId,
        origin: Option<&ModOrigin>,
        locked: bool,
        cx: &Context<Self>,
    ) -> Option<Div> {
        let origin = match NexusOrigin::read(origin?) {
            Ok(found) => found?,
            Err(err) => {
                tracing::warn!(mod_id = %id, %err, "nexus origin of a mod does not parse");
                return None;
            }
        };
        let state = self.nexus_updates.get(id);
        let checking = matches!(state, Some(UpdateCheck::Checking));
        let check_id = id.clone();
        let check_origin = origin.clone();
        let mod_id = origin.mod_id;
        let result = state.map(|state| match state {
            UpdateCheck::Checking => theme::muted(t!("mods.nexus.checking")),
            UpdateCheck::UpToDate => theme::muted(t!("mods.nexus.up_to_date")),
            UpdateCheck::Gone => theme::muted(t!("mods.nexus.gone")),
            UpdateCheck::Failed(text) => theme::error(text),
            UpdateCheck::Newer(file) => div()
                .flex()
                .items_center()
                .gap_3()
                .child(theme::muted(t!("mods.nexus.newer", file = file)))
                .child(
                    button(
                        ("nx-open-update", index),
                        ButtonKind::Secondary,
                        t!("mods.nexus.show"),
                        locked,
                    )
                    .when(!locked, |b| {
                        b.on_click(cx.listener(move |this, _, _, cx| this.open_nexus(Some(mod_id), cx)))
                    }),
                ),
        });
        Some(
            div()
                .flex()
                .flex_col()
                .gap_1()
                .pl(px(29.))
                .text_size(px(12.5))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_3()
                        .child(theme::muted(t!(
                            "mods.nexus.origin",
                            domain = origin.domain,
                            mod_id = origin.mod_id,
                            file_id = origin.file_id,
                            version = origin.version
                        )))
                        // The check asks Nexus with the personal key, which only the experimental
                        // switch allows; the mod itself keeps working without it.
                        .children(self.nexus_experimental.then(|| {
                            button(
                                ("nx-check-update", index),
                                ButtonKind::Secondary,
                                t!("mods.nexus.check_updates"),
                                locked || checking,
                            )
                            .when(!locked && !checking, |b| {
                                b.on_click(cx.listener(move |this, _, _, cx| {
                                    this.check_nexus_update(check_id.clone(), check_origin.clone(), cx);
                                }))
                            })
                        })),
                )
                .children(result),
        )
    }

    fn check_nexus_update(&mut self, id: ModId, origin: NexusOrigin, cx: &mut Context<Self>) {
        self.nexus_updates.insert(id.clone(), UpdateCheck::Checking);
        cx.notify();
        cx.spawn(async move |this, cx| {
            let checked = tokio::spawn(async move {
                let client = crate::nexus::client().await?;
                let list = decanter_nexus::files(&client, &origin.domain, origin.mod_id).await?;
                // The credits are read again on the same press; failing to only keeps the old ones.
                let credits = match decanter_nexus::mod_info(&client, &origin.domain, origin.mod_id).await {
                    Ok(info) => Some(info.credits(&origin.domain)),
                    Err(err) => {
                        tracing::warn!(mod_id = %origin.mod_id, %err, "nexus mod not read again for its credits");
                        None
                    }
                };
                let state = match decanter_nexus::update(&list, origin.file_id) {
                    Update::Newer(file) => {
                        tracing::info!(
                            mod_id = %origin.mod_id,
                            installed = %origin.file_id,
                            latest = %file.id,
                            "nexus update available"
                        );
                        UpdateCheck::Newer(match file.version.as_deref() {
                            Some(version) => format!("{} ({version})", file.name),
                            None => file.name.clone(),
                        })
                    }
                    Update::Gone => {
                        tracing::info!(mod_id = %origin.mod_id, "nexus mod has no current file any more");
                        UpdateCheck::Gone
                    }
                    Update::UpToDate | _ => {
                        tracing::info!(mod_id = %origin.mod_id, file_id = %origin.file_id, "nexus mod up to date");
                        UpdateCheck::UpToDate
                    }
                };
                Ok::<_, decanter_nexus::Error>((state, credits))
            })
            .await;
            let _ = this.update(cx, |this, cx| {
                let state = match checked {
                    Ok(Ok((state, credits))) => {
                        if let Some(credits) = credits {
                            this.refresh_credits(id.clone(), credits, cx);
                        }
                        state
                    }
                    Ok(Err(err)) => {
                        tracing::warn!(%err, "nexus update check failed");
                        UpdateCheck::Failed(failure_text(&err))
                    }
                    Err(err) => {
                        tracing::error!(%err, "nexus update check stopped");
                        UpdateCheck::Failed(Text::verbatim(err))
                    }
                };
                this.nexus_updates.insert(id, state);
                cx.notify();
            });
        })
        .detach();
    }
}
