//! *Buscar actualizaciones* on a mod that came from mod.io: the file its `mod.toml` origin names
//! against the file mod.io offers now, asked only when the user presses it, and only for a game
//! whose own record still allows third-party access.

use decanter_modio::{FileId, Origin};
use decanter_mods::{ModId, ModOrigin};
use gpui::{Context, Div, div, prelude::*, px};
use rust_i18n::t;

use super::ModsTab;
use crate::i18n::Text;
use crate::theme;
use crate::widgets::{ButtonKind, button};

pub(in crate::views::library::properties::mods) enum UpdateCheck {
    Checking,
    UpToDate,
    Newer {
        version: Option<String>,
    },
    /// mod.io offers no file for the mod any more.
    NoFile,
    Failed(Text),
}

impl ModsTab {
    /// The origin line of a mod downloaded from mod.io, with its update check.
    pub(in crate::views::library::properties::mods) fn modio_origin_block(
        &self,
        index: usize,
        id: &ModId,
        origin: Option<&ModOrigin>,
        locked: bool,
        cx: &Context<Self>,
    ) -> Option<Div> {
        let stored = origin?;
        let origin = Origin::from_mod_origin(stored)?;
        let version = Origin::version_of(stored).unwrap_or("-").to_owned();
        let state = self.modio_updates.get(id);
        let checking = matches!(state, Some(UpdateCheck::Checking));
        let check_id = id.clone();
        let result = state.map(|state| match state {
            UpdateCheck::Checking => theme::muted(t!("mods.modio.checking")),
            UpdateCheck::UpToDate => theme::muted(t!("mods.modio.up_to_date")),
            UpdateCheck::NoFile => theme::muted(t!("mods.modio.gone")),
            UpdateCheck::Failed(text) => theme::error(text),
            UpdateCheck::Newer { version } => div()
                .flex()
                .items_center()
                .gap_3()
                .child(theme::muted(t!(
                    "mods.modio.newer",
                    version = version.as_deref().unwrap_or("-")
                )))
                .child(
                    button(
                        ("modio-open-update", index),
                        ButtonKind::Secondary,
                        t!("mods.modio.show"),
                        locked,
                    )
                    .when(!locked, |b| {
                        b.on_click(cx.listener(move |this, _, _, cx| this.open_modio(Some(origin.mod_id), cx)))
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
                            "mods.modio.origin",
                            mod_id = origin.mod_id,
                            file = origin.file,
                            version = version,
                            host = stored.host
                        )))
                        .child(
                            button(
                                ("modio-check-update", index),
                                ButtonKind::Secondary,
                                t!("mods.modio.check_updates"),
                                locked || checking,
                            )
                            .when(!locked && !checking, |b| {
                                b.on_click(cx.listener(move |this, _, _, cx| {
                                    this.modio_check_update(check_id.clone(), origin, cx);
                                }))
                            }),
                        ),
                )
                .children(result),
        )
    }

    fn modio_check_update(&mut self, id: ModId, origin: Origin, cx: &mut Context<Self>) {
        self.modio_updates.insert(id.clone(), UpdateCheck::Checking);
        cx.notify();
        cx.spawn(async move |this, cx| {
            let checked = tokio::spawn(current_file(origin))
                .await
                .unwrap_or_else(|err| Err(Text::verbatim(err)));
            let _ = this.update(cx, |this, cx| {
                let state = match checked {
                    // mod.io documents no order between files, so any other current file is offered,
                    // a rollback by the author included.
                    Ok(Some((current, version))) if current != origin.file => {
                        tracing::info!(mod_id = %id, installed = %origin.file, current = %current, "modio update available");
                        UpdateCheck::Newer { version }
                    }
                    Ok(Some((current, _))) => {
                        tracing::info!(mod_id = %id, installed = %origin.file, current = %current, "modio mod up to date");
                        UpdateCheck::UpToDate
                    }
                    Ok(None) => {
                        tracing::info!(mod_id = %id, "modio mod offers no file any more");
                        UpdateCheck::NoFile
                    }
                    Err(text) => {
                        tracing::warn!(mod_id = %id, error = %text, "modio update check failed");
                        UpdateCheck::Failed(text)
                    }
                };
                this.modio_updates.insert(id, state);
                cx.notify();
            });
        })
        .detach();
    }
}

/// The file mod.io offers now for the mod `origin` names, once the game's record still allows
/// third-party access.
async fn current_file(origin: Origin) -> Result<Option<(FileId, Option<String>)>, Text> {
    let client = tokio::task::spawn_blocking(crate::modio::client)
        .await
        .unwrap_or_else(|err| Err(Text::verbatim(err)))?;
    let failure = |err: decanter_modio::Error| crate::modio::failure(&err);
    let game = decanter_modio::game(&client, origin.game).await.map_err(failure)?;
    decanter_modio::check_access(&game).map_err(failure)?;
    let remote = decanter_modio::fetch_mod(&client, origin.game, origin.mod_id)
        .await
        .map_err(failure)?;
    Ok(remote.current.map(|file| (file.id, file.version)))
}
