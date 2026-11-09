//! The game's third-party mods: add a folder of loose files or a zip or 7z archive, turn each on
//! or off for this account, see which files they replace, and remove one after saying what goes.
//! Changes apply at the next launch, never while the game runs (docs/third-party-mods-design.md).

mod archive;
mod attention;
mod dll;
mod nexus;
mod thunderstore;

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;
use std::rc::Rc;

use decanter_mods::{EffectiveOverride, GameMods, ModConflicts, ModId, ModManifest, ModPath, Shift};
use gpui::{Context, Entity, ExternalPaths, SharedString, Window, div, prelude::*};
use rust_i18n::t;

use super::{Target, error_line, field_row, not_installed, pane};
use crate::i18n::Text;
use crate::theme;
use crate::widgets::input::TextInput;
use crate::widgets::{ButtonKind, button, checkbox, confirm_panel, tooltip};
use archive::Importing;
use nexus::Browser as NexusBrowser;
use thunderstore::{Browser, UpdateCheck};

/// How many paths an expanded conflict lists before summing up the rest.
const SHOWN_PATHS: usize = 50;
/// How many of the game's own files in the play tree are named before summing up the rest.
const SHOWN_WRITTEN: usize = 5;

struct Row {
    manifest: ModManifest,
    enabled: bool,
    /// Its place in this account's load order, when enabled.
    position: Option<usize>,
    /// The game files an update changed under it, which leave it out until kept or disabled.
    attention: Option<Vec<ModPath>>,
    /// Accounts whose set has it enabled.
    users: usize,
    conflicts: Option<ModConflicts>,
}

struct Loaded {
    /// Enabled ones first, in load order, then the rest as they were added.
    rows: Vec<Row>,
    names: HashMap<ModId, String>,
    written_by_game: Vec<ModPath>,
    enabled_count: usize,
    /// What the enabled mods ask Wine for, the later one winning a DLL.
    dll: Vec<EffectiveOverride>,
}

pub(super) struct ModsTab {
    target: Rc<Target>,
    mods: Option<GameMods>,
    loaded: Option<Loaded>,
    running: bool,
    busy: Option<Text>,
    expanded: HashSet<ModId>,
    confirm_remove: Option<ModId>,
    /// An archive being read, previewed or extracted.
    importing: Option<Importing>,
    /// The previewed archive's mod name, editable before adding.
    name_input: Entity<TextInput>,
    /// A plain line about the last action, such as a cancelled import.
    notice: Option<Text>,
    /// Per mod, the files that are no longer what was imported, once checked.
    verified: HashMap<ModId, Vec<ModPath>>,
    /// The Thunderstore browser, while open.
    thunderstore: Option<Browser>,
    /// Per mod from Thunderstore, its last update check.
    updates: HashMap<ModId, UpdateCheck>,
    /// The Nexus Mods browser, while open.
    nexus: Option<NexusBrowser>,
    /// Per mod from Nexus Mods, its last update check, only ever asked by its button.
    nexus_updates: HashMap<ModId, nexus::UpdateCheck>,
    error: Option<Text>,
}

impl ModsTab {
    pub(super) fn new(target: Rc<Target>, cx: &mut Context<Self>) -> Self {
        let name_input = cx.new(|cx| TextInput::new(Text::new("mods.archive.name"), cx));
        let mut this = Self {
            target,
            mods: None,
            loaded: None,
            running: false,
            busy: None,
            expanded: HashSet::new(),
            confirm_remove: None,
            importing: None,
            name_input,
            notice: None,
            verified: HashMap::new(),
            thunderstore: None,
            updates: HashMap::new(),
            nexus: None,
            nexus_updates: HashMap::new(),
            error: None,
        };
        this.reload(cx);
        this
    }

    fn reload(&mut self, cx: &Context<Self>) {
        let account_id = self.target.account_id.clone();
        let installed = crate::core::installed_game(&account_id, &self.target.product_id);
        self.mods = installed.as_ref().and_then(crate::mods::for_game);
        let build_id = installed.map(|game| game.build_id).unwrap_or_default();
        self.running = crate::launch::is_running(&account_id, &self.target.product_id);
        let Some(mods) = self.mods.clone() else {
            return;
        };
        cx.spawn(async move |this, cx| {
            let read = tokio::task::spawn_blocking(move || snapshot(&mods, &account_id, &build_id)).await;
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
        self.notice = None;
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

    fn shift(&mut self, id: ModId, step: Shift, cx: &mut Context<Self>) {
        self.run(
            Text::new("library.properties.mods.applying"),
            move |mods, account| mods.shift(account, &id, step),
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
            let _ = this.update(cx, |this, cx| this.add_folder(source, cx));
        })
        .detach();
    }

    fn add_folder(&mut self, source: PathBuf, cx: &mut Context<Self>) {
        tracing::info!(path = %source.display(), "mod folder picked");
        self.run(
            Text::new("library.properties.mods.adding"),
            move |mods, account| mods.import_folder(&source, account).map(|_| ()),
            cx,
        );
    }

    /// A folder dropped on the tab is added as loose files, anything else read as an archive.
    fn dropped(&mut self, paths: &ExternalPaths, cx: &mut Context<Self>) {
        if self.is_locked() {
            tracing::info!("drop on the mods tab ignored, it is busy or the game runs");
            return;
        }
        let Some(path) = paths.paths().first().cloned() else {
            return;
        };
        if paths.paths().len() > 1 {
            tracing::info!(
                count = paths.paths().len(),
                "several files dropped on the mods tab, only the first is added"
            );
        }
        if path.is_dir() {
            self.add_folder(path, cx);
        } else {
            self.read_archive(path, cx);
        }
    }

    fn is_locked(&self) -> bool {
        self.running
            || self.busy.is_some()
            || self.importing.is_some()
            || self.thunderstore.as_ref().is_some_and(Browser::is_busy)
            || self.nexus.as_ref().is_some_and(NexusBrowser::is_busy)
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
            .children(
                row.position
                    .map(|position| self.order_buttons(index, &id, position, locked, cx)),
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
            .children(self.dll_block(index, row, locked, cx))
            .children(self.attention_block(index, row, locked, cx))
            .children(self.verify_block(index, row, locked, cx))
            .children(self.origin_block(index, &row.manifest.id, row.manifest.origin.as_ref(), locked, cx))
            .children(self.nexus_origin_block(index, &row.manifest.id, row.manifest.origin.as_ref(), locked, cx))
    }

    /// Up and down in the load order, where the later mod wins a file and a DLL.
    fn order_buttons(&self, index: usize, id: &ModId, position: usize, locked: bool, cx: &Context<Self>) -> gpui::Div {
        let count = self.loaded.as_ref().map_or(0, |loaded| loaded.enabled_count);
        let first = position == 0 || locked;
        let last = position.saturating_add(1) >= count || locked;
        let (earlier, later) = (id.clone(), id.clone());
        div()
            .flex()
            .gap_1()
            .child(
                button(("mod-earlier", index), ButtonKind::Icon, "↑", first)
                    .tooltip(tooltip(t!("mods.order.earlier")))
                    .when(!first, |b| {
                        b.on_click(cx.listener(move |this, _, _, cx| this.shift(earlier.clone(), Shift::Earlier, cx)))
                    }),
            )
            .child(
                button(("mod-later", index), ButtonKind::Icon, "↓", last)
                    .tooltip(tooltip(t!("mods.order.later")))
                    .when(!last, |b| {
                        b.on_click(cx.listener(move |this, _, _, cx| this.shift(later.clone(), Shift::Later, cx)))
                    }),
            )
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
fn snapshot(mods: &GameMods, account_id: &str, build_id: &str) -> decanter_mods::Result<Loaded> {
    let all = mods.list()?;
    let mut attention = mods.needing_attention(account_id, build_id)?;
    let enabled = mods.enabled(account_id)?;
    let mut conflicts = mods.conflicts(account_id)?;
    let dll = mods.dll_overrides(account_id)?;
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
            position: order.get(&manifest.id).copied(),
            attention: attention.remove(&manifest.id),
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
        enabled_count: enabled.len(),
        dll,
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
        let locked = self.is_locked();
        let add_label = self.busy.as_ref().map_or_else(
            || SharedString::from(t!("library.properties.mods.add").into_owned()),
            SharedString::from,
        );
        let actions = field_row()
            .items_center()
            .child(
                button(
                    "mod-add-archive",
                    ButtonKind::Secondary,
                    t!("mods.archive.add_button"),
                    locked,
                )
                .when(!locked, |b| {
                    b.on_click(cx.listener(|_, _, _, cx| Self::pick_archive(cx)))
                }),
            )
            .child(
                button("mod-add", ButtonKind::Secondary, add_label, locked)
                    .when(!locked, |b| b.on_click(cx.listener(|_, _, _, cx| Self::add(cx)))),
            )
            .child(
                button(
                    "mod-add-thunderstore",
                    ButtonKind::Secondary,
                    t!("mods.thunderstore.add_button"),
                    locked || self.thunderstore.is_some(),
                )
                .when(!locked && self.thunderstore.is_none(), |b| {
                    b.on_click(cx.listener(|this, _, _, cx| this.open_thunderstore(None, cx)))
                }),
            )
            .child(
                button(
                    "mod-add-nexus",
                    ButtonKind::Secondary,
                    t!("mods.nexus.add_button"),
                    locked || self.nexus.is_some(),
                )
                .when(!locked && self.nexus.is_none(), |b| {
                    b.on_click(cx.listener(|this, _, _, cx| this.open_nexus(None, cx)))
                }),
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
            .on_drop(cx.listener(|this, paths: &ExternalPaths, _, cx| this.dropped(paths, cx)))
            .child(theme::muted(t!("library.properties.mods.note")))
            .child(theme::muted(t!("mods.archive.note")))
            .child(actions)
            .children(self.archive_panel(cx))
            .children(self.thunderstore_panel(locked, cx))
            .children(self.nexus_panel(locked, cx))
            .children(self.notice.as_ref().map(theme::muted))
            .children(self.confirmation(cx))
            .child(list)
            .child(theme::muted(t!("mods.order.later_wins")))
            .children(written)
            .children(error_line(self.error.as_ref()))
    }
}
