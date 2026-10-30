//! A game's settings as a whole, on its Resumen: copy them from another game (all of them or one
//! tab's), export them to one file, import one, or put the game back to the defaults for new games.
//! Each replacement asks first and backs the game's files up before writing
//! ([`decanter_ps::bbport::per_game`]); the tabs read the files again when shown.

use std::path::PathBuf;

use decanter_ps::bbport::per_game::{self, Category, PerGameError, Target};
use decanter_ps::bbport::transfer;
use gpui::{Context, Entity, EventEmitter, SharedString, Window, div, prelude::*};
use rust_i18n::t;

use super::display_title;
use crate::i18n::Text;
use crate::state::{Ps, per_game_error};
use crate::theme;
use crate::widgets::{ButtonKind, Select, button, confirm_panel, select::SelectStyle};

/// The value the category list gives every category at once.
const ALL: &str = "all";

/// Emitted once a game's files were replaced, so cached tabs read them again.
pub struct Changed;

enum Pending {
    Copy,
    Reset,
    /// A file already checked; `other` is the title it holds when that isn't this one.
    Import {
        path: PathBuf,
        other: Option<String>,
    },
}

pub struct TitleSettingsView {
    ps: Entity<Ps>,
    title_id: String,
    /// Other titles with settings of their own, to copy from.
    sources: Vec<String>,
    source: Option<String>,
    /// `None` for every category.
    category: Option<Category>,
    pending: Option<Pending>,
    is_busy: bool,
    message: Option<(Text, bool)>,
}

impl EventEmitter<Changed> for TitleSettingsView {}

impl TitleSettingsView {
    pub fn new(ps: Entity<Ps>, title_id: String, cx: &mut Context<Self>) -> Self {
        let this = Self {
            ps,
            title_id,
            sources: Vec::new(),
            source: None,
            category: None,
            pending: None,
            is_busy: false,
            message: None,
        };
        this.reload_sources(cx);
        this
    }

    fn reload_sources(&self, cx: &mut Context<Self>) {
        let own = self.title_id.clone();
        cx.spawn(async move |this, cx| {
            let titles = cx
                .background_spawn(async { per_game::titles(&decanter_ps::paths::active()) })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.sources = titles.into_iter().filter(|title| *title != own).collect();
                if this.source.as_ref().is_none_or(|source| !this.sources.contains(source)) {
                    this.source = this.sources.first().cloned();
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Runs `job` off the UI thread and says how it went; `done` words the success.
    fn run(
        &mut self,
        job: impl FnOnce(&decanter_ps::paths::Root) -> Result<Option<PathBuf>, PerGameError> + Send + 'static,
        done: Text,
        cx: &mut Context<Self>,
    ) {
        self.pending = None;
        self.is_busy = true;
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move { job(&decanter_ps::paths::active()) })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.is_busy = false;
                this.message = Some(match result {
                    Ok(Some(backup)) => (
                        Text::new("ps.per_game.done_with_backup")
                            .text("done", done)
                            .arg("path", backup.display()),
                        false,
                    ),
                    Ok(None) => (done, false),
                    Err(err) => {
                        tracing::warn!(error = %err, "game settings not changed");
                        (per_game_error(&err), true)
                    }
                });
                cx.emit(Changed);
                this.reload_sources(cx);
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn copy(&mut self, cx: &mut Context<Self>) {
        let Some(source) = self.source.clone() else {
            return;
        };
        let (title_id, category) = (self.title_id.clone(), self.category);
        let done = Text::new("ps.per_game.copied").arg("game", self.name(&source, cx));
        self.run(
            move |root| {
                let categories = category.map_or_else(|| Category::ALL.to_vec(), |category| vec![category]);
                let (from, to) = (Target::parse(root, &source)?, Target::parse(root, &title_id)?);
                per_game::copy(root, &from, &to, &categories).map(Some)
            },
            done,
            cx,
        );
    }

    fn reset(&mut self, cx: &mut Context<Self>) {
        let title_id = self.title_id.clone();
        self.run(
            move |root| per_game::reset(root, &Target::parse(root, &title_id)?).map(Some),
            Text::new("ps.per_game.reset_done"),
            cx,
        );
    }

    fn export(&self, cx: &mut Context<Self>) {
        let title_id = self.title_id.clone();
        cx.spawn(async move |this, cx| {
            let Some(picked) = rfd::AsyncFileDialog::new()
                .set_title(t!("ps.per_game.export_title"))
                .set_file_name(format!("{title_id}-settings.json"))
                .add_filter("JSON", &["json"])
                .save_file()
                .await
            else {
                return;
            };
            let path = picked.path().to_path_buf();
            let _ = this.update(cx, |this, cx| {
                let done = Text::new("ps.per_game.exported").arg("path", path.display());
                this.run(
                    move |root| {
                        let text = transfer::export(root, &Target::parse(root, &title_id)?)?;
                        std::fs::write(&path, text)?;
                        Ok(None)
                    },
                    done,
                    cx,
                );
            });
        })
        .detach();
    }

    /// Checks the picked file first, so the question names whose settings it holds.
    fn pick_import(&self, cx: &mut Context<Self>) {
        let title_id = self.title_id.clone();
        cx.spawn(async move |this, cx| {
            let Some(picked) = rfd::AsyncFileDialog::new()
                .set_title(t!("ps.per_game.import_title"))
                .add_filter("JSON", &["json"])
                .pick_file()
                .await
            else {
                return;
            };
            let path = picked.path().to_path_buf();
            let checked = cx
                .background_spawn({
                    let path = path.clone();
                    async move {
                        let root = decanter_ps::paths::active();
                        transfer::check_file(&root, &Target::parse(&root, &title_id)?, &path, false)
                    }
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                match checked {
                    Ok(()) => this.pending = Some(Pending::Import { path, other: None }),
                    Err(PerGameError::OtherTitle { file, .. }) => {
                        this.pending = Some(Pending::Import {
                            path,
                            other: Some(file),
                        });
                    }
                    Err(err) => this.message = Some((per_game_error(&err), true)),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn import(&mut self, path: PathBuf, is_any_title: bool, cx: &mut Context<Self>) {
        let title_id = self.title_id.clone();
        self.run(
            move |root| transfer::import_file(root, &Target::parse(root, &title_id)?, &path, is_any_title).map(Some),
            Text::new("ps.per_game.imported"),
            cx,
        );
    }

    fn name(&self, title_id: &str, cx: &Context<Self>) -> String {
        self.ps
            .read(cx)
            .entries()
            .iter()
            .find(|entry| entry.game.title_id == title_id)
            .map_or_else(|| title_id.to_owned(), |entry| display_title(&entry.game))
    }

    fn confirmation(&self, pending: &Pending, cx: &Context<Self>) -> impl IntoElement {
        let (title, body, yes) = match pending {
            Pending::Copy => {
                let source = self.source.as_deref().unwrap_or_default();
                let part = self.category.map_or_else(
                    || t!("ps.per_game.categories.all").into_owned(),
                    |category| category_label(category).into_owned(),
                );
                (
                    t!("ps.per_game.confirm_copy.title"),
                    t!(
                        "ps.per_game.confirm_copy.body",
                        part = part,
                        game = self.name(source, cx)
                    ),
                    t!("ps.per_game.confirm_copy.yes"),
                )
            }
            Pending::Reset => (
                t!("ps.per_game.confirm_reset.title"),
                t!("ps.per_game.confirm_reset.body"),
                t!("ps.per_game.confirm_reset.yes"),
            ),
            Pending::Import { other: None, .. } => (
                t!("ps.per_game.confirm_import.title"),
                t!("ps.per_game.confirm_import.body"),
                t!("ps.per_game.confirm_import.yes"),
            ),
            Pending::Import { other: Some(file), .. } => (
                t!("ps.per_game.confirm_import.title"),
                t!("ps.per_game.confirm_import.other_body", game = self.name(file, cx)),
                t!("ps.per_game.confirm_import.other_yes"),
            ),
        };
        let entity = cx.entity();
        let cancel = cx.entity();
        confirm_panel(
            "ps-title-settings-confirm",
            title,
            body,
            yes,
            move |_, _, cx| {
                entity.update(cx, |this, cx| match this.pending.take() {
                    Some(Pending::Copy) => this.copy(cx),
                    Some(Pending::Reset) => this.reset(cx),
                    Some(Pending::Import { path, other }) => this.import(path, other.is_some(), cx),
                    None => {}
                });
            },
            move |_, _, cx| {
                cancel.update(cx, |this, cx| {
                    this.pending = None;
                    cx.notify();
                });
            },
        )
    }

    fn copy_row(&self, cx: &Context<Self>) -> impl IntoElement {
        let names = self.sources.iter().map(|source| {
            (
                SharedString::from(source.clone()),
                SharedString::from(self.name(source, cx)),
            )
        });
        let source_entity = cx.entity();
        let sources = Select::new(
            "ps-title-settings-source",
            names,
            SharedString::from(self.source.clone().unwrap_or_default()),
        )
        .style(SelectStyle::Compact)
        .on_change(move |value, _, cx| {
            let value = value.to_string();
            source_entity.update(cx, |this, cx| {
                this.source = Some(value);
                cx.notify();
            });
        });
        let parts = std::iter::once((
            SharedString::from(ALL),
            SharedString::from(t!("ps.per_game.categories.all")),
        ))
        .chain(Category::ALL.into_iter().map(|category| {
            (
                SharedString::from(category.id()),
                SharedString::from(category_label(category)),
            )
        }));
        let part_entity = cx.entity();
        let categories = Select::new(
            "ps-title-settings-category",
            parts,
            SharedString::from(self.category.map_or(ALL, Category::id)),
        )
        .style(SelectStyle::Compact)
        .on_change(move |value, _, cx| {
            let category = Category::parse(value);
            part_entity.update(cx, |this, cx| {
                this.category = category;
                cx.notify();
            });
        });
        let is_disabled = self.is_busy || self.source.is_none();
        theme::compact_row(false)
            .gap_3()
            .child(div().flex_1().child(SharedString::from(t!("ps.per_game.copy_from"))))
            .child(sources)
            .child(categories)
            .child(
                button(
                    "ps-title-settings-copy",
                    ButtonKind::Secondary,
                    t!("ps.per_game.copy"),
                    is_disabled,
                )
                .when(!is_disabled, |copy| {
                    copy.on_click(cx.listener(|this, _, _, cx| {
                        this.pending = Some(Pending::Copy);
                        cx.notify();
                    }))
                }),
            )
    }
}

fn category_label(category: Category) -> std::borrow::Cow<'static, str> {
    match category {
        Category::Graphics => t!("ps.per_game.categories.graphics"),
        Category::Keyboard => t!("ps.per_game.categories.keyboard"),
        Category::Gamepad => t!("ps.per_game.categories.gamepad"),
        Category::Mods => t!("ps.per_game.categories.mods"),
        Category::Online => t!("ps.per_game.categories.online"),
        Category::Extras => t!("ps.per_game.categories.extras"),
    }
}

impl Render for TitleSettingsView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let is_busy = self.is_busy;
        let copy = if self.sources.is_empty() {
            theme::compact_row(false)
                .child(theme::muted(t!("ps.per_game.no_sources")))
                .into_any_element()
        } else {
            self.copy_row(cx).into_any_element()
        };
        let actions = theme::compact_row(true)
            .gap_2()
            .child(
                button(
                    "ps-title-settings-export",
                    ButtonKind::Secondary,
                    t!("ps.per_game.export"),
                    is_busy,
                )
                .when(!is_busy, |export| {
                    export.on_click(cx.listener(|this, _, _, cx| this.export(cx)))
                }),
            )
            .child(
                button(
                    "ps-title-settings-import",
                    ButtonKind::Secondary,
                    t!("ps.per_game.import"),
                    is_busy,
                )
                .when(!is_busy, |import| {
                    import.on_click(cx.listener(|this, _, _, cx| this.pick_import(cx)))
                }),
            )
            .child(
                button(
                    "ps-title-settings-reset",
                    ButtonKind::Secondary,
                    t!("ps.per_game.reset"),
                    is_busy,
                )
                .when(!is_busy, |reset| {
                    reset.on_click(cx.listener(|this, _, _, cx| {
                        this.pending = Some(Pending::Reset);
                        cx.notify();
                    }))
                }),
            );
        div()
            .flex()
            .flex_col()
            .gap_2()
            .child(theme::h3(t!("ps.per_game.title")))
            .child(theme::card().child(copy).child(actions))
            .child(theme::muted(t!("ps.per_game.description")))
            .children(self.pending.as_ref().map(|pending| self.confirmation(pending, cx)))
            .children(self.message.as_ref().map(|(text, is_error)| {
                if *is_error {
                    theme::error(text)
                } else {
                    theme::muted(text)
                }
            }))
    }
}
