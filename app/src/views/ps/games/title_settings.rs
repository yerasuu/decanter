//! A game's settings as a whole, on its Resumen: copy them from another game (all of them or one
//! tab's, also opened from each tab, [`TitleSettingsView::open_copy`]), apply them to every other
//! game of the port, export them to one file, import one, put the game back to the defaults for new
//! games, or forget them. Each replacement asks first and backs the game's files up before writing
//! ([`decanter_ps::bbport::per_game`]); the tabs read the files again when shown.

use std::path::{Path, PathBuf};

use decanter_ps::bbport::per_game::{self, Applied, Category, Destination, PerGameError, Target};
use decanter_ps::bbport::transfer;
use decanter_ps::port::Port;
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

#[derive(Clone, Copy, PartialEq, Eq)]
enum Work {
    Idle,
    /// Reading which games play online, before "apply to all" asks.
    Planning,
    Writing,
}

enum Pending {
    Copy,
    /// Every other game of the port, with what each gets.
    Spread(Vec<Destination>),
    Reset,
    Forget,
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
    /// What "apply to all" copies; `None` for every category.
    spread_category: Option<Category>,
    /// Whether the game plays online; without it the Online category is left out of copies.
    has_online: bool,
    /// Shown inside a tab, by its entry: the copy row alone, preselected to the tab's category.
    is_copy_only: bool,
    pending: Option<Pending>,
    work: Work,
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
            spread_category: None,
            has_online: false,
            is_copy_only: false,
            pending: None,
            work: Work::Idle,
            message: None,
        };
        this.reload_sources(cx);
        this
    }

    pub fn set_has_online(&mut self, has_online: bool, cx: &mut Context<Self>) {
        self.has_online = has_online;
        if !has_online && self.category == Some(Category::Online) {
            self.category = None;
        }
        if !has_online && self.spread_category == Some(Category::Online) {
            self.spread_category = None;
        }
        cx.notify();
    }

    /// The copy row alone, for a tab's "copy settings from another game" entry, with that tab's
    /// category picked.
    pub fn open_copy(&mut self, category: Category, cx: &mut Context<Self>) {
        self.category = Some(category);
        self.is_copy_only = true;
        self.pending = None;
        self.message = None;
        cx.notify();
    }

    /// Back to the whole of it, as the Resumen shows it.
    pub fn close_copy(&mut self, cx: &mut Context<Self>) {
        if self.is_copy_only {
            self.is_copy_only = false;
            self.pending = None;
            cx.notify();
        }
    }

    fn is_busy(&self) -> bool {
        self.work != Work::Idle
    }

    pub const fn is_copy_only(&self) -> bool {
        self.is_copy_only
    }

    fn categories(&self) -> impl Iterator<Item = Category> {
        let has_online = self.has_online;
        Category::ALL
            .into_iter()
            .filter(move |&category| has_online || category != Category::Online)
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
        self.work = Work::Writing;
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move { job(&decanter_ps::paths::active()) })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.work = Work::Idle;
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
        let all: Vec<Category> = self.categories().collect();
        let done = Text::new("ps.per_game.copied").arg("game", self.name(&source, cx));
        self.run(
            move |root| {
                let categories = category.map_or_else(|| all.clone(), |category| vec![category]);
                let (from, to) = (Target::parse(root, &source)?, Target::parse(root, &title_id)?);
                per_game::copy(root, &from, &to, &categories).map(Some)
            },
            done,
            cx,
        );
    }

    /// The port's other games in the library, by title id and folder.
    fn other_games(&self, cx: &Context<Self>) -> Vec<(String, PathBuf)> {
        self.ps
            .read(cx)
            .entries()
            .iter()
            .filter(|entry| entry.port == Some(Port::Bbport) && entry.game.title_id != self.title_id)
            .map(|entry| (entry.game.title_id.clone(), entry.game.dir.clone()))
            .collect()
    }

    /// Finds out off the UI thread which games get what, the online ones read from their eboots,
    /// so the question says how many.
    fn plan_spread(&mut self, cx: &mut Context<Self>) {
        let games = self.other_games(cx);
        let title_id = self.title_id.clone();
        let categories: Vec<Category> = self
            .spread_category
            .map_or_else(|| self.categories().collect(), |category| vec![category]);
        self.work = Work::Planning;
        self.message = None;
        cx.spawn(async move |this, cx| {
            let plan = cx
                .background_spawn(async move {
                    let root = decanter_ps::paths::active();
                    per_game::plan_to_all(&root, &Target::parse(&root, &title_id)?, &games, &categories)
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.work = Work::Idle;
                match plan {
                    Ok(plan) if plan.iter().any(|destination| !destination.categories.is_empty()) => {
                        this.pending = Some(Pending::Spread(plan));
                    }
                    Ok(_) => this.message = Some((Text::new("ps.per_game.nothing_to_apply"), false)),
                    Err(err) => this.message = Some((per_game_error(&err), true)),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn spread(&mut self, plan: Vec<Destination>, cx: &mut Context<Self>) {
        self.pending = None;
        self.work = Work::Writing;
        let title_id = self.title_id.clone();
        cx.spawn(async move |this, cx| {
            let applied = cx
                .background_spawn(async move {
                    let root = decanter_ps::paths::active();
                    per_game::copy_to_all(&root, &Target::parse(&root, &title_id)?, &plan)
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.work = Work::Idle;
                this.message = Some(match applied {
                    Ok(applied) => spread_message(&applied),
                    Err(err) => {
                        tracing::warn!(error = %err, "game settings not applied to every game");
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

    fn forget(&mut self, cx: &mut Context<Self>) {
        let title_id = self.title_id.clone();
        self.run(
            move |root| per_game::forget(root, &Target::parse(root, &title_id)?).map(Some),
            Text::new("ps.per_game.forgotten"),
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
                (
                    t!("ps.per_game.confirm_copy.title"),
                    t!(
                        "ps.per_game.confirm_copy.body",
                        part = part_label(self.category),
                        game = self.name(source, cx)
                    ),
                    t!("ps.per_game.confirm_copy.yes"),
                )
            }
            Pending::Spread(plan) => {
                let count = plan
                    .iter()
                    .filter(|destination| !destination.categories.is_empty())
                    .count();
                let without_online = plan.iter().filter(|destination| destination.is_without_online).count();
                let mut body = t!(
                    "ps.per_game.confirm_all.body",
                    part = part_label(self.spread_category),
                    count = count
                )
                .into_owned();
                if without_online > 0 {
                    body.push(' ');
                    body.push_str(&t!("ps.per_game.confirm_all.without_online", count = without_online));
                }
                (
                    t!("ps.per_game.confirm_all.title"),
                    body.into(),
                    t!("ps.per_game.confirm_all.yes", count = count),
                )
            }
            Pending::Forget => (
                t!("ps.per_game.confirm_forget.title"),
                t!("ps.per_game.confirm_forget.body"),
                t!("ps.per_game.confirm_forget.yes"),
            ),
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
                    Some(Pending::Spread(plan)) => this.spread(plan, cx),
                    Some(Pending::Reset) => this.reset(cx),
                    Some(Pending::Forget) => this.forget(cx),
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
        let part_entity = cx.entity();
        let categories = self.category_select("ps-title-settings-category", self.category, move |category, cx| {
            part_entity.update(cx, |this, cx| {
                this.category = category;
                cx.notify();
            });
        });
        let is_disabled = self.is_busy() || self.source.is_none();
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

    /// Export, import, reset and forget.
    fn actions(&self, cx: &Context<Self>) -> impl IntoElement {
        let is_busy = self.is_busy();
        theme::compact_row(true)
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
            )
            .child(
                button(
                    "ps-title-settings-forget",
                    ButtonKind::Secondary,
                    t!("ps.per_game.forget"),
                    is_busy,
                )
                .when(!is_busy, |forget| {
                    forget.on_click(cx.listener(|this, _, _, cx| {
                        this.pending = Some(Pending::Forget);
                        cx.notify();
                    }))
                }),
            )
    }

    /// "All settings", then each category this game offers.
    fn category_select(
        &self,
        id: &'static str,
        picked: Option<Category>,
        on_change: impl Fn(Option<Category>, &mut gpui::App) + 'static,
    ) -> Select {
        let parts = std::iter::once((
            SharedString::from(ALL),
            SharedString::from(t!("ps.per_game.categories.all")),
        ))
        .chain(self.categories().map(|category| {
            (
                SharedString::from(category.id()),
                SharedString::from(category_label(category)),
            )
        }));
        Select::new(id, parts, SharedString::from(picked.map_or(ALL, Category::id)))
            .style(SelectStyle::Compact)
            .on_change(move |value, _, cx| on_change(Category::parse(value), cx))
    }

    fn spread_row(&self, cx: &Context<Self>) -> impl IntoElement {
        if self.other_games(cx).is_empty() {
            return theme::compact_row(false)
                .child(theme::muted(t!("ps.per_game.no_destinations")))
                .into_any_element();
        }
        let entity = cx.entity();
        let categories = self.category_select(
            "ps-title-settings-spread-category",
            self.spread_category,
            move |category, cx| {
                entity.update(cx, |this, cx| {
                    this.spread_category = category;
                    cx.notify();
                });
            },
        );
        theme::compact_row(false)
            .gap_3()
            .child(
                div()
                    .flex_1()
                    .child(SharedString::from(t!("ps.per_game.apply_all_label"))),
            )
            .child(categories)
            .child(
                button(
                    "ps-title-settings-spread",
                    ButtonKind::Secondary,
                    t!("ps.per_game.apply_all"),
                    self.is_busy(),
                )
                .when(!self.is_busy(), |spread| {
                    spread.on_click(cx.listener(|this, _, _, cx| this.plan_spread(cx)))
                }),
            )
            .into_any_element()
    }
}

/// How "apply to all" went: how many games got it and where the backups went, or the first that
/// failed.
fn spread_message(applied: &[Applied]) -> (Text, bool) {
    let done: Vec<&Path> = applied
        .iter()
        .filter_map(|applied| applied.result.as_ref().ok())
        .map(PathBuf::as_path)
        .collect();
    let failed = applied.iter().find_map(|applied| {
        applied
            .result
            .as_ref()
            .err()
            .map(|err| (applied.title_id.as_str(), err))
    });
    let backups = done
        .first()
        .and_then(|backup| backup.parent())
        .unwrap_or_else(|| Path::new(""));
    match failed {
        None => (
            Text::new("ps.per_game.applied_all")
                .arg("count", done.len())
                .arg("path", backups.display()),
            false,
        ),
        Some((title, err)) => (
            Text::new("ps.per_game.applied_some")
                .arg("count", done.len())
                .arg("failed", applied.len().saturating_sub(done.len()))
                .arg("title", title)
                .text("error", per_game_error(err)),
            true,
        ),
    }
}

fn part_label(category: Option<Category>) -> String {
    category.map_or_else(
        || t!("ps.per_game.categories.all").into_owned(),
        |category| category_label(category).into_owned(),
    )
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
        let copy = if self.sources.is_empty() {
            theme::compact_row(self.is_copy_only)
                .child(theme::muted(t!("ps.per_game.no_sources")))
                .into_any_element()
        } else {
            self.copy_row(cx).into_any_element()
        };
        let pending = self.pending.as_ref().map(|pending| self.confirmation(pending, cx));
        let message = self.message.as_ref().map(|(text, is_error)| {
            if *is_error {
                theme::error(text)
            } else {
                theme::muted(text)
            }
        });
        if self.is_copy_only {
            return div()
                .flex()
                .flex_col()
                .gap_2()
                .child(
                    theme::card().child(copy).child(
                        theme::compact_row(true).child(div().flex_1()).child(
                            button(
                                "ps-title-settings-close",
                                ButtonKind::Secondary,
                                t!("ps.per_game.close"),
                                false,
                            )
                            .on_click(cx.listener(|this, _, _, cx| this.close_copy(cx))),
                        ),
                    ),
                )
                .children(pending)
                .children(message)
                .into_any_element();
        }
        let actions = self.actions(cx);
        let planning = (self.work == Work::Planning).then(|| theme::muted(t!("ps.per_game.planning")));
        div()
            .flex()
            .flex_col()
            .gap_2()
            .child(theme::h3(t!("ps.per_game.title")))
            .child(theme::card().child(copy).child(self.spread_row(cx)).child(actions))
            .child(theme::muted(t!("ps.per_game.description")))
            .children(pending)
            .children(planning)
            .children(message)
            .into_any_element()
    }
}
