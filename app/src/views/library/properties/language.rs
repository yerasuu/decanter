//! Idioma: which language an installed GOG game is in (#1355). Changing it
//! downloads only the new language's depots; the files of the languages it no
//! longer uses are then listed with their size and removed only once the user
//! confirms (#1133). A game installed before languages were chosen has every
//! language on disk, and the same list frees them.

use std::rc::Rc;

use decanter_core::InstalledGame;
use decanter_gog::LanguageCleanup;
use gpui::{Context, SharedString, Window, div, prelude::*};
use rust_i18n::t;

use super::{Target, error_line, field_row, not_installed, pane, row, rows};
use crate::i18n::Text;
use crate::state::{AppState, DownloadKey, DownloadKind};
use crate::theme;
use crate::views::library::install::{self, DownloadJob, Ended};
use crate::widgets::select::Select;
use crate::widgets::{ButtonKind, button};

/// How many of the files to remove are named; the rest are counted.
const LISTED_FILES: usize = 8;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Busy {
    Switching,
    Using,
    Listing,
    Removing,
}

pub(super) struct LanguageTab {
    target: Rc<Target>,
    installed: Option<InstalledGame>,
    /// The languages the installed build splits its files by; `None` while
    /// they load.
    offered: Option<Result<Vec<String>, Text>>,
    picked: Option<String>,
    busy: Option<Busy>,
    /// The dry run waiting for the user's confirmation.
    cleanup: Option<LanguageCleanup>,
    freed: Option<u64>,
    error: Option<Text>,
}

impl LanguageTab {
    pub(super) fn new(target: Rc<Target>, cx: &Context<Self>) -> Self {
        let installed = crate::core::installed_game(&target.account_id, &target.product_id);
        let this = Self {
            target,
            installed,
            offered: None,
            picked: None,
            busy: None,
            cleanup: None,
            freed: None,
            error: None,
        };
        if let Some(game) = &this.installed {
            this.load(game, cx);
        }
        this
    }

    fn reload(&mut self) {
        self.installed = crate::core::installed_game(&self.target.account_id, &self.target.product_id);
    }

    fn load(&self, game: &InstalledGame, cx: &Context<Self>) {
        let target = Rc::clone(&self.target);
        let (branch, build_id) = (game.branch.clone(), game.build_id.clone());
        cx.spawn(async move |this, cx| {
            let outcome =
                decanter_gog::offered_languages(&target.account_id, &target.product_id, branch.as_deref(), Some(&build_id))
                    .await
                    .inspect_err(|err| {
                        tracing::error!(account_id = target.account_id, product_id = target.product_id, error = %err, "failed to read the languages of the installed build");
                    })
                    .map_err(Text::verbatim);
            let wanted = crate::core::gog_languages(&target.account_id, &target.product_id);
            let _ = this.update(cx, |this, cx| {
                if let Ok(offered) = &outcome {
                    let current = this.installed.as_ref().and_then(|game| game.language.clone());
                    this.picked = current.or_else(|| {
                        decanter_gog::language::resolve(&wanted, offered).map(|picked| picked.tag)
                    });
                }
                this.offered = Some(outcome);
                cx.notify();
            });
        })
        .detach();
    }

    fn switch(&mut self, tag: String, cx: &mut Context<Self>) {
        let target = Rc::clone(&self.target);
        self.busy = Some(Busy::Switching);
        self.error = None;
        self.cleanup = None;
        self.freed = None;
        tracing::info!(
            account_id = target.account_id,
            product_id = target.product_id,
            language = tag,
            "language change started"
        );
        cx.notify();
        let state = AppState::get(cx).clone();
        cx.spawn(async move |this, cx| {
            let account_id = target.account_id.as_str();
            let product_id = target.product_id.as_str();
            let Ok(core_library) = crate::core::library_for_account(account_id) else {
                tracing::error!(
                    account_id,
                    product_id,
                    "could not resolve library, language change aborted"
                );
                let _ = this.update(cx, |this, cx| {
                    this.error = Some(Text::new("library.install.no_library"));
                    this.busy = None;
                    cx.notify();
                });
                return;
            };
            crate::core::set_gog_language(account_id, product_id, &tag);
            let (on_progress, progress) = install::progress_channel();
            let change = decanter_gog::add_language(&core_library, account_id, product_id, &tag, on_progress);
            let job = DownloadJob {
                key: DownloadKey::new(account_id, product_id),
                title: target.title.clone(),
                kind: DownloadKind::Language,
            };
            let outcome = install::track(&state, job, progress, change, cx).await;
            let is_done = matches!(outcome, Ended::Done(()));
            let _ = this.update(cx, |this, cx| {
                this.busy = None;
                if let Ended::Failed(error) = outcome {
                    this.error = Some(error);
                }
                this.reload();
                if is_done {
                    tracing::info!(account_id, product_id, language = tag, "language change finished");
                    this.list_unused(tag, cx);
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Records the language of a game that has every language on disk: nothing is downloaded or
    /// removed.
    fn use_language(&mut self, tag: String, cx: &mut Context<Self>) {
        let target = Rc::clone(&self.target);
        self.busy = Some(Busy::Using);
        self.error = None;
        self.cleanup = None;
        self.freed = None;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let account_id = target.account_id.as_str();
            let product_id = target.product_id.as_str();
            let outcome = match crate::core::library_for_account(account_id) {
                Ok(core_library) => decanter_gog::use_language(&core_library, account_id, product_id, &tag)
                    .await
                    .inspect_err(|err| tracing::error!(account_id, product_id, language = tag, error = %err, "recording the game's language failed"))
                    .map_err(Text::verbatim),
                Err(err) => Err(err.localized()),
            };
            if outcome.is_ok() {
                crate::core::set_gog_language(account_id, product_id, &tag);
            }
            let _ = this.update(cx, |this, cx| {
                this.busy = None;
                if let Err(error) = outcome {
                    this.error = Some(error);
                }
                this.reload();
                cx.notify();
            });
        })
        .detach();
    }

    /// The dry run: what removing the languages other than `keep` would delete.
    fn list_unused(&mut self, keep: String, cx: &mut Context<Self>) {
        let target = Rc::clone(&self.target);
        self.busy = Some(Busy::Listing);
        self.error = None;
        self.cleanup = None;
        self.freed = None;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let account_id = target.account_id.as_str();
            let product_id = target.product_id.as_str();
            let outcome = match crate::core::library_for_account(account_id) {
                Ok(core_library) => {
                    decanter_gog::unused_language_files(&core_library, account_id, product_id, &[keep])
                        .await
                        .inspect_err(|err| tracing::error!(account_id, product_id, error = %err, "listing the files of unused languages failed"))
                        .map_err(Text::verbatim)
                }
                Err(err) => Err(err.localized()),
            };
            let _ = this.update(cx, |this, cx| {
                this.busy = None;
                match outcome {
                    // A game installed before languages were chosen that has nothing to free still
                    // gets its language recorded, which is what the user asked for.
                    Ok(cleanup) if cleanup.files.is_empty() && cleanup.missing == 0 && cleanup.language.is_some() => {
                        this.remove(cleanup, cx);
                    }
                    Ok(cleanup) => this.cleanup = Some(cleanup),
                    Err(error) => this.error = Some(error),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn remove(&mut self, cleanup: LanguageCleanup, cx: &mut Context<Self>) {
        let target = Rc::clone(&self.target);
        self.busy = Some(Busy::Removing);
        self.error = None;
        self.cleanup = None;
        tracing::info!(
            account_id = target.account_id,
            product_id = target.product_id,
            language = cleanup.language,
            file_count = cleanup.files.len(),
            bytes = cleanup.total_bytes,
            "removing the files of unused languages, confirmed"
        );
        cx.notify();
        cx.spawn(async move |this, cx| {
            let account_id = target.account_id.as_str();
            let product_id = target.product_id.as_str();
            let outcome = match crate::core::library_for_account(account_id) {
                Ok(core_library) => {
                    decanter_gog::remove_unused_languages(&core_library, account_id, product_id, &cleanup)
                        .await
                        .inspect_err(|err| tracing::error!(account_id, product_id, error = %err, "removing the files of unused languages failed"))
                        .map_err(Text::verbatim)
                }
                Err(err) => Err(err.localized()),
            };
            if outcome.is_ok()
                && let Some(tag) = &cleanup.language
            {
                crate::core::set_gog_language(account_id, product_id, tag);
            }
            let _ = this.update(cx, |this, cx| {
                this.busy = None;
                match outcome {
                    Ok(freed) => this.freed = Some(freed),
                    Err(error) => this.error = Some(error),
                }
                this.reload();
                cx.notify();
            });
        })
        .detach();
    }

    fn picker(&self, offered: &[String], cx: &Context<Self>) -> impl IntoElement + use<> {
        let options = offered.iter().map(|tag| {
            (
                SharedString::from(tag.clone()),
                SharedString::from(decanter_gog::language::native_name(tag)),
            )
        });
        let this = cx.entity().downgrade();
        Select::new("game-language", options, self.picked.clone().unwrap_or_default()).on_change(move |value, _, cx| {
            let _ = this.update(cx, |this, cx| {
                this.picked = Some(value.to_string());
                this.cleanup = None;
                this.freed = None;
                cx.notify();
            });
        })
    }

    fn cleanup_panel(cleanup: &LanguageCleanup, cx: &Context<Self>) -> impl IntoElement + use<> {
        let kept = cleanup
            .language
            .as_deref()
            .map(decanter_gog::language::native_name)
            .unwrap_or_default();
        let mut panel = theme::card().p_4().flex().flex_col().gap_2();
        if cleanup.missing > 0 {
            return panel.child(theme::error(t!(
                "library.properties.language.missing",
                count = cleanup.missing,
                language = kept
            )));
        }
        if cleanup.files.is_empty() {
            return panel.child(theme::muted(t!("library.properties.language.nothing")));
        }
        panel =
            panel
                .child(div().child(t!(
                    "library.properties.language.confirm",
                    count = cleanup.files.len(),
                    size = crate::format::bytes(cleanup.total_bytes),
                    language = kept
                )))
                .children(
                    cleanup.files.iter().take(LISTED_FILES).map(|file| {
                        theme::muted(format!("{} · {}", file.relative_path, crate::format::bytes(file.size)))
                    }),
                )
                .children((cleanup.files.len() > LISTED_FILES).then(|| {
                    theme::muted(t!(
                        "library.properties.language.more",
                        count = cleanup.files.len() - LISTED_FILES
                    ))
                }))
                .child(theme::muted(t!("library.properties.language.safe")));
        let confirmed = cleanup.clone();
        panel.child(
            field_row()
                .child(
                    button(
                        "language-remove-confirm",
                        ButtonKind::Primary,
                        t!(
                            "library.properties.language.remove_confirm",
                            size = crate::format::bytes(cleanup.total_bytes)
                        ),
                        false,
                    )
                    .on_click(cx.listener(move |this, _, _, cx| this.remove(confirmed.clone(), cx))),
                )
                .child(
                    button(
                        "language-remove-cancel",
                        ButtonKind::Secondary,
                        t!("common.cancel"),
                        false,
                    )
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.cleanup = None;
                        cx.notify();
                    })),
                ),
        )
    }
}

impl Render for LanguageTab {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(installed) = &self.installed else {
            return not_installed();
        };
        let current = installed.language.as_deref().map_or_else(
            || t!("library.properties.language.all").into_owned(),
            decanter_gog::language::native_name,
        );
        let mut body = pane().child(rows().child(row(t!("library.properties.language.installed"), current)));
        let offered = match &self.offered {
            None => return body.child(theme::muted(t!("common.loading"))),
            Some(Err(error)) => return body.child(theme::error(error.clone())),
            Some(Ok(offered)) => offered.clone(),
        };
        if offered.is_empty() {
            return body.child(theme::muted(t!("library.properties.language.not_split")));
        }
        // A download of the game shares its staging folder with a language change, which would
        // take it over, and its files are what a removal reads.
        let key = DownloadKey::new(&self.target.account_id, &self.target.product_id);
        let is_downloading =
            self.busy != Some(Busy::Switching) && AppState::get(cx).background.read(cx).installing.contains_key(&key);
        let is_busy = self.busy.is_some() || is_downloading;
        let picked = self.picked.clone();
        let changes = picked.is_some() && installed.language.is_some() && picked != installed.language;
        let mut actions = field_row().child(self.picker(&offered, cx));
        if changes && let Some(tag) = picked.clone() {
            let label = if self.busy == Some(Busy::Switching) {
                t!("library.properties.language.switching")
            } else {
                t!("library.properties.language.switch")
            };
            actions = actions.child(
                button("language-switch", ButtonKind::Primary, label, is_busy).when(!is_busy, |b| {
                    b.on_click(cx.listener(move |this, _, _, cx| this.switch(tag.clone(), cx)))
                }),
            );
        } else if let Some(keep) = picked {
            let label = match self.busy {
                Some(Busy::Listing) => t!("library.properties.language.listing"),
                Some(Busy::Removing) => t!("library.properties.language.removing"),
                Some(Busy::Switching | Busy::Using) | None => t!("library.properties.language.remove_unused"),
            };
            if installed.language.is_none() {
                let tag = keep.clone();
                let label = if self.busy == Some(Busy::Using) {
                    t!("library.properties.language.switching")
                } else {
                    t!("library.properties.language.use")
                };
                actions = actions.child(
                    button("language-use", ButtonKind::Primary, label, is_busy).when(!is_busy, |b| {
                        b.on_click(cx.listener(move |this, _, _, cx| this.use_language(tag.clone(), cx)))
                    }),
                );
            }
            actions = actions.child(
                button("language-unused", ButtonKind::Secondary, label, is_busy).when(!is_busy, |b| {
                    b.on_click(cx.listener(move |this, _, _, cx| this.list_unused(keep.clone(), cx)))
                }),
            );
        }
        body = body
            .child(theme::muted(t!("library.properties.language.note")))
            .child(actions)
            .children(is_downloading.then(|| theme::muted(t!("library.properties.language.downloading"))));
        if let Some(cleanup) = self.cleanup.as_ref() {
            if !cleanup.is_wanted {
                body = body.child(theme::muted(t!(
                    "library.properties.language.fallback",
                    language = cleanup
                        .language
                        .as_deref()
                        .map(decanter_gog::language::native_name)
                        .unwrap_or_default()
                )));
            }
            body = body.child(Self::cleanup_panel(cleanup, cx));
        }
        body.children(self.freed.map(|freed| {
            theme::muted(t!(
                "library.properties.language.freed",
                size = crate::format::bytes(freed)
            ))
        }))
        .children(error_line(self.error.as_ref()))
    }
}
