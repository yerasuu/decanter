//! The game's third-party mods: add a folder of loose files, turn each on or off for this
//! account, see which files they replace, and remove one after saying what goes. Changes apply at
//! the next launch, never while the game runs (docs/third-party-mods-design.md, phase 1).

use std::collections::{BTreeMap, HashMap, HashSet};
use std::rc::Rc;

use decanter_mods::{GameMods, ModConflicts, ModId, ModManifest, ModPath};
use gpui::{Context, SharedString, Window, div, prelude::*};
use rust_i18n::t;

use super::{Target, error_line, field_row, not_installed, pane};
use crate::i18n::Text;
use crate::theme;
use crate::widgets::{ButtonKind, button, checkbox, confirm_panel};

/// How many paths an expanded conflict lists before summing up the rest.
const SHOWN_PATHS: usize = 50;
/// How many of the game's own files in the play tree are named before summing up the rest.
const SHOWN_WRITTEN: usize = 5;

struct Row {
    manifest: ModManifest,
    enabled: bool,
    /// Accounts whose set has it enabled.
    users: usize,
    conflicts: Option<ModConflicts>,
}

struct Loaded {
    /// Enabled ones first, in load order, then the rest as they were added.
    rows: Vec<Row>,
    names: HashMap<ModId, String>,
    written_by_game: Vec<ModPath>,
}

pub(super) struct ModsTab {
    target: Rc<Target>,
    mods: Option<GameMods>,
    loaded: Option<Loaded>,
    running: bool,
    busy: Option<Text>,
    expanded: HashSet<ModId>,
    confirm_remove: Option<ModId>,
    error: Option<Text>,
}

impl ModsTab {
    pub(super) fn new(target: Rc<Target>, cx: &Context<Self>) -> Self {
        let mut this = Self {
            target,
            mods: None,
            loaded: None,
            running: false,
            busy: None,
            expanded: HashSet::new(),
            confirm_remove: None,
            error: None,
        };
        this.reload(cx);
        this
    }

    fn reload(&mut self, cx: &Context<Self>) {
        let account_id = self.target.account_id.clone();
        self.mods = crate::core::installed_game(&account_id, &self.target.product_id)
            .as_ref()
            .and_then(crate::mods::for_game);
        self.running = crate::launch::is_running(&account_id, &self.target.product_id);
        let Some(mods) = self.mods.clone() else {
            return;
        };
        cx.spawn(async move |this, cx| {
            let read = tokio::task::spawn_blocking(move || snapshot(&mods, &account_id)).await;
            let _ = this.update(cx, |this, cx| {
                match read {
                    Ok(Ok(loaded)) => this.loaded = Some(loaded),
                    Ok(Err(err)) => {
                        tracing::error!(error = %err, "could not read the game's mods");
                        this.error = Some(Text::verbatim(err));
                    }
                    Err(err) => {
                        tracing::error!(error = %err, "reading the game's mods stopped");
                        this.error = Some(Text::verbatim(err));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Runs `work` off the UI with every action disabled meanwhile, then reads the mods again.
    fn run(
        &mut self,
        busy: Text,
        work: impl FnOnce(&GameMods, &str) -> decanter_mods::Result<()> + Send + 'static,
        cx: &mut Context<Self>,
    ) {
        let Some(mods) = self.mods.clone() else {
            return;
        };
        let account_id = self.target.account_id.clone();
        self.busy = Some(busy);
        self.error = None;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let outcome = tokio::task::spawn_blocking(move || work(&mods, &account_id)).await;
            let _ = this.update(cx, |this, cx| {
                this.busy = None;
                match outcome {
                    Ok(Ok(())) => {}
                    Ok(Err(err)) => {
                        tracing::error!(error = %err, "mod change failed");
                        this.error = Some(Text::verbatim(err));
                    }
                    Err(err) => {
                        tracing::error!(error = %err, "mod change stopped");
                        this.error = Some(Text::verbatim(err));
                    }
                }
                this.reload(cx);
            });
        })
        .detach();
    }

    fn toggle(&mut self, id: ModId, enabled: bool, cx: &mut Context<Self>) {
        self.run(
            Text::new("library.properties.mods.applying"),
            move |mods, account| mods.set_enabled(account, &id, enabled),
            cx,
        );
    }

    fn add(cx: &Context<Self>) {
        cx.spawn(async move |this, cx| {
            let Some(folder) = rfd::AsyncFileDialog::new()
                .set_title(t!("library.properties.mods.pick_title"))
                .pick_folder()
                .await
            else {
                return;
            };
            let source = folder.path().to_path_buf();
            tracing::info!(path = %source.display(), "mod folder picked");
            let _ = this.update(cx, |this, cx| {
                this.run(
                    Text::new("library.properties.mods.adding"),
                    move |mods, account| mods.import_folder(&source, account).map(|_| ()),
                    cx,
                );
            });
        })
        .detach();
    }

    fn remove(&mut self, id: ModId, cx: &mut Context<Self>) {
        self.confirm_remove = None;
        self.run(
            Text::new("library.properties.mods.removing"),
            move |mods, _| mods.remove(&id),
            cx,
        );
    }

    fn mod_row(&self, index: usize, row: &Row, last: bool, locked: bool, cx: &Context<Self>) -> impl IntoElement {
        let names = self.loaded.as_ref().map(|loaded| &loaded.names);
        let name_of = |id: &ModId| -> String {
            names
                .and_then(|names| names.get(id).cloned())
                .unwrap_or_else(|| id.to_string())
        };
        let id = row.manifest.id.clone();
        let enabled = row.enabled;
        let toggle_id = id.clone();
        let remove_id = id.clone();
        let expand_id = id.clone();
        let expanded = self.expanded.contains(&id);
        let summary = t!(
            "library.properties.mods.summary",
            files = row.manifest.files.len(),
            size = crate::format::bytes(row.manifest.size())
        );

        let header = div()
            .flex()
            .items_center()
            .gap_4()
            .child(
                div()
                    .id(("mod-toggle", index))
                    .when(!locked, |toggle| {
                        toggle
                            .cursor_pointer()
                            .on_click(cx.listener(move |this, _, _, cx| this.toggle(toggle_id.clone(), !enabled, cx)))
                    })
                    .child(checkbox(enabled).when(locked, |b| b.opacity(0.4))),
            )
            .child(div().flex_1().child(row.manifest.name.clone()))
            .child(theme::muted(summary))
            .child(
                button(
                    ("mod-remove", index),
                    ButtonKind::Icon,
                    t!("library.properties.mods.remove"),
                    locked,
                )
                .when(!locked, |b| {
                    b.on_click(cx.listener(move |this, _, _, cx| {
                        this.confirm_remove = Some(remove_id.clone());
                        cx.notify();
                    }))
                }),
            );

        let lines = row
            .conflicts
            .as_ref()
            .map(|conflicts| conflict_lines(conflicts, &name_of))
            .unwrap_or_default();
        let has_conflicts = !lines.is_empty();
        let details = has_conflicts.then(|| {
            let toggle_label = if expanded {
                t!("library.properties.mods.hide_files")
            } else {
                t!("library.properties.mods.show_files")
            };
            div()
                .flex()
                .flex_col()
                .gap_1()
                .pl(gpui::px(29.))
                .text_size(gpui::px(12.5))
                .children(lines.into_iter().map(|(label, paths)| {
                    div()
                        .flex()
                        .flex_col()
                        .child(theme::muted(label))
                        .when(expanded, |block| block.children(path_list(paths)))
                }))
                .child(
                    div()
                        .id(("mod-expand", index))
                        .cursor_pointer()
                        .text_color(gpui::rgb(theme::ACCENT_BLUE))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if !this.expanded.remove(&expand_id) {
                                this.expanded.insert(expand_id.clone());
                            }
                            cx.notify();
                        }))
                        .child(toggle_label),
                )
        });

        div()
            .flex()
            .flex_col()
            .gap_2()
            .px(gpui::px(18.))
            .py(gpui::px(12.))
            .when(!last, |item| item.border_b_1().border_color(gpui::rgb(theme::LINE)))
            .child(header)
            .children(details)
    }

    fn confirmation(&self, cx: &Context<Self>) -> Option<impl IntoElement> {
        let id = self.confirm_remove.clone()?;
        let row = self.loaded.as_ref()?.rows.iter().find(|row| row.manifest.id == id)?;
        let body = t!(
            "library.properties.mods.remove_body",
            size = crate::format::bytes(row.manifest.size()),
            accounts = row.users
        );
        Some(confirm_panel(
            "mod-remove-confirm",
            t!("library.properties.mods.remove_title", name = row.manifest.name),
            body,
            t!("library.properties.mods.remove"),
            cx.listener(move |this, _, _, cx| this.remove(id.clone(), cx)),
            cx.listener(|this, _, _, cx| {
                this.confirm_remove = None;
                cx.notify();
            }),
        ))
    }
}

/// What the tab shows, read off the UI: every mod of the game with this account's choice, and the
/// conflicts among the enabled ones.
fn snapshot(mods: &GameMods, account_id: &str) -> decanter_mods::Result<Loaded> {
    let all = mods.list()?;
    let enabled = mods.enabled(account_id)?;
    let mut conflicts = mods.conflicts(account_id)?;
    let written_by_game = mods.written_by_game(account_id)?;
    let names = all
        .iter()
        .map(|manifest| (manifest.id.clone(), manifest.name.clone()))
        .collect();
    let order: BTreeMap<&ModId, usize> = enabled.iter().enumerate().map(|(index, id)| (id, index)).collect();
    let mut rows = Vec::with_capacity(all.len());
    for manifest in all {
        let users = mods.users(&manifest.id)?.len();
        rows.push(Row {
            enabled: order.contains_key(&manifest.id),
            users,
            conflicts: conflicts.remove(&manifest.id),
            manifest,
        });
    }
    rows.sort_by_key(|row| order.get(&row.manifest.id).copied().unwrap_or(usize::MAX));
    Ok(Loaded {
        rows,
        names,
        written_by_game,
    })
}

/// One line per kind of conflict of a mod, each with the paths it is about.
fn conflict_lines<'row>(
    conflicts: &'row ModConflicts,
    name_of: &dyn Fn(&ModId) -> String,
) -> Vec<(SharedString, &'row [ModPath])> {
    let mut lines: Vec<(SharedString, &[ModPath])> = Vec::new();
    if !conflicts.overrides_game.is_empty() {
        lines.push((
            t!(
                "library.properties.mods.overrides_game",
                count = conflicts.overrides_game.len()
            )
            .into_owned()
            .into(),
            conflicts.overrides_game.as_slice(),
        ));
    }
    for (other, paths) in &conflicts.overrides {
        lines.push((
            t!(
                "library.properties.mods.overrides",
                name = name_of(other),
                count = paths.len()
            )
            .into_owned()
            .into(),
            paths.as_slice(),
        ));
    }
    for (other, paths) in &conflicts.overridden_by {
        lines.push((
            t!(
                "library.properties.mods.overridden_by",
                name = name_of(other),
                count = paths.len()
            )
            .into_owned()
            .into(),
            paths.as_slice(),
        ));
    }
    lines
}

fn path_list(paths: &[ModPath]) -> impl Iterator<Item = gpui::Div> + '_ {
    let rest = paths.len().saturating_sub(SHOWN_PATHS);
    paths
        .iter()
        .take(SHOWN_PATHS)
        .map(|path| theme::mono().pl_2().child(path.to_string()))
        .chain((rest > 0).then(|| theme::muted(t!("library.properties.mods.more", count = rest)).pl_2()))
}

impl Render for ModsTab {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.mods.is_none() {
            return not_installed();
        }
        let locked = self.running || self.busy.is_some();
        let add_label = self.busy.as_ref().map_or_else(
            || SharedString::from(t!("library.properties.mods.add").into_owned()),
            SharedString::from,
        );
        let actions = field_row()
            .items_center()
            .child(
                button("mod-add", ButtonKind::Secondary, add_label, locked)
                    .when(!locked, |b| b.on_click(cx.listener(|_, _, _, cx| Self::add(cx)))),
            )
            .children(
                self.running
                    .then(|| theme::muted(t!("library.properties.mods.running"))),
            );

        let list = match &self.loaded {
            None => theme::muted(t!("library.properties.mods.loading")),
            Some(loaded) if loaded.rows.is_empty() => theme::muted(t!("library.properties.mods.none")),
            Some(loaded) => {
                let count = loaded.rows.len();
                theme::card().children(
                    loaded
                        .rows
                        .iter()
                        .enumerate()
                        .map(|(index, row)| self.mod_row(index, row, index + 1 == count, locked, cx)),
                )
            }
        };

        let written = self
            .loaded
            .as_ref()
            .filter(|loaded| !loaded.written_by_game.is_empty())
            .map(|loaded| {
                let shown = loaded
                    .written_by_game
                    .iter()
                    .take(SHOWN_WRITTEN)
                    .map(ModPath::as_str)
                    .collect::<Vec<_>>()
                    .join(", ");
                theme::muted(format!(
                    "{} {shown}",
                    t!(
                        "library.properties.mods.written_by_game",
                        count = loaded.written_by_game.len()
                    )
                ))
            });

        pane()
            .gap_3()
            .child(theme::muted(t!("library.properties.mods.note")))
            .child(actions)
            .children(self.confirmation(cx))
            .child(list)
            .child(theme::muted(t!("library.properties.mods.later_wins")))
            .children(written)
            .children(error_line(self.error.as_ref()))
    }
}
