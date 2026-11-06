//! Registering an Amazon Games folder the user already has, from the action bar of a game that is
//! not installed.

use std::path::PathBuf;

use decanter_amazon::Differences;
use decanter_core::Game;
use gpui::{AnyElement, Context, prelude::*};
use rust_i18n::t;

use super::DetailView;
use crate::i18n::Text;
use crate::state::{AppState, DownloadKey, DownloadKind};
use crate::theme;
use crate::views::library::amazon_install::{self, Imported};
use crate::widgets::{ButtonKind, button};

#[derive(Default)]
pub(super) struct Import {
    busy: bool,
    error: Option<Text>,
    /// The folder that did not match, kept for the user to accept it and have it repaired.
    differing: Option<Differing>,
}

struct Differing {
    folder: PathBuf,
    title: String,
    count: usize,
    total: usize,
}

impl DetailView {
    pub(super) fn import_action(&self, game: &Game, cx: &Context<Self>) -> Vec<AnyElement> {
        let import = &self.local.import;
        if import.busy {
            return vec![theme::muted(t!("amazon.import.working")).into_any_element()];
        }
        let mut elements = Vec::new();
        if let Some(differing) = &import.differing {
            let (folder, title) = (differing.folder.clone(), differing.title.clone());
            elements.push(
                theme::muted(t!(
                    "amazon.import.differs",
                    differing = differing.count,
                    total = differing.total
                ))
                .into_any_element(),
            );
            elements.push(
                button(
                    "import-repair",
                    ButtonKind::Secondary,
                    t!("amazon.import.register_repair"),
                    false,
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.import_folder(folder.clone(), title.clone(), Differences::Accept, cx);
                }))
                .into_any_element(),
            );
            elements.push(
                button("import-dismiss", ButtonKind::Secondary, t!("common.cancel"), false)
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.local.import.differing = None;
                        cx.notify();
                    }))
                    .into_any_element(),
            );
        } else {
            let title = game.title.clone();
            elements.push(
                button("import", ButtonKind::Secondary, t!("amazon.import.button"), false)
                    .on_click(cx.listener(move |this, _, _, cx| this.pick_import_folder(title.clone(), cx)))
                    .into_any_element(),
            );
        }
        elements.extend(
            import
                .error
                .as_ref()
                .map(|error| theme::error(error).into_any_element()),
        );
        elements
    }

    fn pick_import_folder(&mut self, title: String, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            let Some(folder) = rfd::AsyncFileDialog::new()
                .set_title(t!("amazon.import.pick_title"))
                .pick_folder()
                .await
            else {
                return;
            };
            let _ = this.update(cx, |this, cx| {
                this.import_folder(folder.path().to_path_buf(), title, Differences::Refuse, cx);
            });
        })
        .detach();
    }

    fn import_folder(&mut self, folder: PathBuf, title: String, differences: Differences, cx: &mut Context<Self>) {
        let Some(loaded) = &self.loaded else {
            return;
        };
        let key = DownloadKey::new(&loaded.account_id, &loaded.product_id);
        self.local.import = Import {
            busy: true,
            ..Import::default()
        };
        cx.notify();
        let state = AppState::get(cx).clone();
        // Detached rather than held by the view: it hashes the whole game, so selecting another one
        // mid-way mustn't drop a repair half started.
        cx.spawn(async move |this, cx| {
            let outcome = amazon_install::import(&key, &title, &folder, differences, &state, cx).await;
            let is_repair = matches!(outcome, Ok(Imported::Recorded)) && differences == Differences::Accept;
            let _ = this.update(cx, |this, cx| {
                let still_shown = this
                    .loaded
                    .as_ref()
                    .is_some_and(|loaded| loaded.product_id == key.product_id && loaded.account_id == key.account_id);
                if still_shown {
                    this.local.import = match outcome {
                        Ok(Imported::Recorded) => Import::default(),
                        Ok(Imported::Differs { differing, total }) => Import {
                            differing: Some(Differing {
                                folder,
                                title: title.clone(),
                                count: differing,
                                total,
                            }),
                            ..Import::default()
                        },
                        Err(error) => Import {
                            error: Some(error),
                            ..Import::default()
                        },
                    };
                }
                cx.notify();
            });
            if is_repair {
                amazon_install::download(key, title, DownloadKind::Repair, None, &state, cx).await;
            }
        })
        .detach();
    }
}
