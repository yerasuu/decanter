//! *Buscar actualizaciones* on a mod that came from Thunderstore: its `mod.toml` origin against
//! the latest version in the game's community listing, asked only when the user presses it.

use std::sync::Arc;

use decanter_mods::{ModId, ModOrigin};
use decanter_thunderstore::{Client, GameSettings, PackageId, Refresh, VersionNumber};
use gpui::{Context, Div, div, prelude::*, px};
use rust_i18n::t;

use super::{ModsTab, failure_text};
use crate::i18n::Text;
use crate::theme;
use crate::widgets::{ButtonKind, button};

pub(in crate::views::library::detail::mods) enum UpdateCheck {
    Checking,
    UpToDate,
    Newer(VersionNumber),
    /// The community's listing no longer has the package.
    Gone,
    Failed(Text),
}

impl ModsTab {
    /// The origin line of a mod downloaded from Thunderstore, with its update check.
    pub(in crate::views::library::detail::mods) fn origin_block(
        &self,
        index: usize,
        id: &ModId,
        origin: Option<&ModOrigin>,
        locked: bool,
        cx: &Context<Self>,
    ) -> Option<Div> {
        let origin = origin.filter(|origin| origin.platform == decanter_thunderstore::PLATFORM)?;
        let package = origin.package.parse::<PackageId>().ok()?;
        let version = origin.version.parse::<VersionNumber>().ok()?;
        let state = self.updates.get(id);
        let checking = matches!(state, Some(UpdateCheck::Checking));
        let check_id = id.clone();
        let check_package = package.clone();
        let result = state.map(|state| match state {
            UpdateCheck::Checking => theme::muted(t!("mods.thunderstore.checking")),
            UpdateCheck::UpToDate => theme::muted(t!("mods.thunderstore.up_to_date")),
            UpdateCheck::Gone => theme::muted(t!("mods.thunderstore.gone")),
            UpdateCheck::Failed(text) => theme::error(text),
            UpdateCheck::Newer(latest) => {
                let open = package.clone();
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .child(theme::muted(t!("mods.thunderstore.newer", version = latest)))
                    .child(
                        button(
                            ("ts-open-update", index),
                            ButtonKind::Secondary,
                            t!("mods.thunderstore.show"),
                            locked,
                        )
                        .when(!locked, |b| {
                            b.on_click(
                                cx.listener(move |this, _, _, cx| this.open_thunderstore(Some(open.clone()), cx)),
                            )
                        }),
                    )
            }
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
                            "mods.thunderstore.origin",
                            package = origin.package,
                            version = origin.version,
                            host = origin.host
                        )))
                        .child(
                            button(
                                ("ts-check-update", index),
                                ButtonKind::Secondary,
                                t!("mods.thunderstore.check_updates"),
                                locked || checking,
                            )
                            .when(!locked && !checking, |b| {
                                b.on_click(cx.listener(move |this, _, _, cx| {
                                    this.check_update(check_id.clone(), check_package.clone(), version, cx);
                                }))
                            }),
                        ),
                )
                .children(result),
        )
    }

    fn check_update(&mut self, id: ModId, package: PackageId, version: VersionNumber, cx: &mut Context<Self>) {
        let Some(mods) = self.mods.as_ref() else {
            return;
        };
        let settings = match GameSettings::read(mods) {
            Ok(Some(settings)) => settings,
            Ok(None) => {
                self.updates
                    .insert(id, UpdateCheck::Failed(Text::new("mods.thunderstore.no_community")));
                cx.notify();
                return;
            }
            Err(err) => {
                self.updates.insert(id, UpdateCheck::Failed(failure_text(&err)));
                cx.notify();
                return;
            }
        };
        // The open browser's listing is the same answer, already read.
        let open = self
            .thunderstore
            .as_ref()
            .and_then(|browser| browser.listing.clone())
            .filter(|listing| *listing.community() == settings.community);
        let Some(cache) = decanter_thunderstore::cache_dir(mods) else {
            return;
        };
        let client = match Client::new() {
            Ok(client) => client,
            Err(err) => {
                self.updates.insert(id, UpdateCheck::Failed(failure_text(&err)));
                cx.notify();
                return;
            }
        };
        self.updates.insert(id.clone(), UpdateCheck::Checking);
        cx.notify();
        cx.spawn(async move |this, cx| {
            let checked = tokio::spawn(async move {
                let listing = match open {
                    Some(listing) => listing,
                    None => Arc::new(
                        decanter_thunderstore::listing(&client, &cache, &settings.community, Refresh::IfStale).await?,
                    ),
                };
                // The credits are those of the installed version while the listing still has it.
                let found = listing.get(&package).map(|found| {
                    let credited = found.version(version).unwrap_or(&found.latest);
                    (found.latest.number, decanter_thunderstore::credits(found, credited))
                });
                Ok::<_, decanter_thunderstore::Error>(found)
            })
            .await;
            let _ = this.update(cx, |this, cx| {
                let checked = checked.map(|read| {
                    read.map(|found| {
                        found.map(|(latest, credits)| {
                            this.refresh_credits(id.clone(), credits, cx);
                            latest
                        })
                    })
                });
                let state = match checked {
                    Ok(Ok(Some(latest))) if latest > version => {
                        tracing::info!(mod_id = %id, installed = %version, latest = %latest, "thunderstore update available");
                        UpdateCheck::Newer(latest)
                    }
                    Ok(Ok(Some(latest))) => {
                        tracing::info!(mod_id = %id, installed = %version, latest = %latest, "thunderstore mod up to date");
                        UpdateCheck::UpToDate
                    }
                    Ok(Ok(None)) => {
                        tracing::info!(mod_id = %id, "thunderstore package no longer in its community's listing");
                        UpdateCheck::Gone
                    }
                    Ok(Err(err)) => {
                        tracing::warn!(%err, "thunderstore update check failed");
                        UpdateCheck::Failed(failure_text(&err))
                    }
                    Err(err) => {
                        tracing::error!(%err, "thunderstore update check stopped");
                        UpdateCheck::Failed(Text::verbatim(err))
                    }
                };
                this.updates.insert(id, state);
                cx.notify();
            });
        })
        .detach();
    }
}
