//! Moving games and PS dumps between library folders, several in one go: a
//! queue run the way Descargas runs downloads (#1349), one at a time in the
//! order shown, reorderable while it waits. A game goes through
//! [`crate::relocate::move_game`], a dump through [`Ps::move_dump`], as a
//! single move of either always does. A destination that is gone or full
//! stops the moves still waiting, which stay where they are.

use std::borrow::Cow;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use decanter_core::InstalledGame;
use gpui::{AnyElement, Context, SharedString, Task, div, prelude::*, px};
use rust_i18n::t;

use super::{Dump, StorageView};
use crate::external::transfer::Progress;
use crate::i18n::Text;
use crate::state::Ps;
use crate::theme;
use crate::widgets::reveal::reveal;
use crate::widgets::select::Select;
use crate::widgets::{ButtonKind, button, checkbox, progress_bar, tooltip};

/// One move per game and account, as the library files a game per account,
/// or per PS dump, by the folder it is in.
#[derive(Clone, PartialEq, Eq, Hash)]
enum MoveKey {
    Game { account_id: String, game_id: String },
    Dump(PathBuf),
}

impl MoveKey {
    fn of(game: &InstalledGame) -> Self {
        Self::Game {
            account_id: game.account_id.clone(),
            game_id: game.game_id.clone(),
        }
    }

    /// For the log: a dump belongs to no account.
    fn account_id(&self) -> &str {
        match self {
            Self::Game { account_id, .. } => account_id,
            Self::Dump(_) => "",
        }
    }

    /// For the log: a dump is named by its folder.
    fn game_id(&self) -> Cow<'_, str> {
        match self {
            Self::Game { game_id, .. } => Cow::Borrowed(game_id),
            Self::Dump(dir) => dir.to_string_lossy(),
        }
    }
}

/// A game or dump picked to move, with the folder it is in.
struct Movable {
    key: MoveKey,
    title: String,
    from: PathBuf,
    size: u64,
}

impl Movable {
    /// `None` for a game a launcher installed, which moves with its launcher.
    fn game(game: &InstalledGame, from: &Path) -> Option<Self> {
        game.prefix_of.is_none().then(|| Self {
            key: MoveKey::of(game),
            title: game.name.clone(),
            from: from.to_owned(),
            size: game.size_on_disk.unwrap_or(0),
        })
    }

    fn dump(dump: &Dump, from: &Path) -> Self {
        Self {
            key: MoveKey::Dump(dump.dir.clone()),
            title: dump.name.clone(),
            from: from.to_owned(),
            size: dump.size,
        }
    }
}

enum MoveStatus {
    Queued,
    Moving(Arc<Progress>),
    Moved,
    /// Never started: already in the destination, stopped by the user, or
    /// left behind when the queue stopped.
    Skipped(Text),
    Failed(Text),
}

impl MoveStatus {
    const fn is_live(&self) -> bool {
        matches!(self, Self::Queued | Self::Moving(_))
    }
}

struct MoveJob {
    key: MoveKey,
    title: String,
    from: PathBuf,
    to: PathBuf,
    /// What has to fit in `to`: nothing for a move within one disk, which
    /// is a rename.
    needed: u64,
    status: MoveStatus,
}

#[derive(Clone, Copy)]
enum Step {
    Up,
    Down,
}

/// The job that holds the turn, as the blocking thread needs it.
struct Turn {
    key: MoveKey,
    from: PathBuf,
    to: PathBuf,
    needed: u64,
    progress: Arc<Progress>,
}

enum Outcome {
    Moved,
    Failed(Text),
    /// Failed, and nothing else can land in this destination either.
    Halted {
        err: Text,
        reason: Text,
    },
}

#[derive(Default)]
pub struct Moves {
    /// In the order they run; finished ones stay until cleared, as the
    /// per-game result.
    jobs: Vec<MoveJob>,
    selected: HashSet<MoveKey>,
}

impl Moves {
    fn is_busy(&self) -> bool {
        self.jobs.iter().any(|job| job.status.is_live())
    }

    fn is_moving(&self) -> bool {
        self.jobs.iter().any(|job| matches!(job.status, MoveStatus::Moving(_)))
    }

    fn live(&self, key: &MoveKey) -> Option<&MoveJob> {
        self.jobs.iter().find(|job| &job.key == key && job.status.is_live())
    }

    fn queued_position(&self, key: &MoveKey) -> Option<usize> {
        self.jobs
            .iter()
            .filter(|job| matches!(job.status, MoveStatus::Queued))
            .position(|job| &job.key == key)
    }

    /// Queues `items` for `to`. Refused as a whole when what has to be copied
    /// there, with what is already queued for it, clearly won't fit.
    fn enqueue(&mut self, items: &[Movable], to: &Path) -> Result<(), Text> {
        let fresh: Vec<&Movable> = items.iter().filter(|item| self.live(&item.key).is_none()).collect();
        let queued_room: u64 = self
            .jobs
            .iter()
            .filter(|job| job.status.is_live() && job.to == to)
            .map(|job| job.needed)
            .sum();
        let needed = fresh
            .iter()
            .map(|item| room_needed(item, to))
            .fold(queued_room, u64::saturating_add);
        if let Some((free, _)) = decanter_core::disk_usage(to)
            && needed > free
        {
            tracing::warn!(to = %to.display(), needed, free, "refusing to queue moves that don't fit");
            return Err(Text::new("storage.queue.errors.no_room")
                .arg("path", to.display())
                .arg("needed", crate::format::bytes(needed))
                .arg("free", crate::format::bytes(free)));
        }
        for item in fresh {
            let already = item.from == to;
            let outcome = if already { "skipped_already_there" } else { "queued" };
            tracing::info!(account_id = item.key.account_id(), game_id = %item.key.game_id(), from = %item.from.display(), to = %to.display(), outcome, "move queued");
            self.jobs.push(MoveJob {
                key: item.key.clone(),
                title: item.title.clone(),
                from: item.from.clone(),
                to: to.to_owned(),
                needed: room_needed(item, to),
                status: if already {
                    MoveStatus::Skipped(Text::new("storage.queue.skipped.already_there"))
                } else {
                    MoveStatus::Queued
                },
            });
        }
        self.selected.clear();
        Ok(())
    }

    /// Hands the turn to the first job waiting, unless one already has it.
    fn take_turn(&mut self) -> Option<Turn> {
        if self.is_moving() {
            return None;
        }
        let job = self
            .jobs
            .iter_mut()
            .find(|job| matches!(job.status, MoveStatus::Queued))?;
        let progress = Arc::new(Progress::default());
        job.status = MoveStatus::Moving(Arc::clone(&progress));
        Some(Turn {
            key: job.key.clone(),
            from: job.from.clone(),
            to: job.to.clone(),
            needed: job.needed,
            progress,
        })
    }

    fn finish(&mut self, turn: &Turn, outcome: Outcome) {
        let halted = match outcome {
            Outcome::Moved => {
                self.set_status(&turn.key, MoveStatus::Moved);
                None
            }
            Outcome::Failed(err) => {
                self.set_status(&turn.key, MoveStatus::Failed(err));
                None
            }
            Outcome::Halted { err, reason } => {
                self.set_status(&turn.key, MoveStatus::Failed(err));
                Some(reason)
            }
        };
        if let Some(reason) = halted {
            self.skip_queued(
                &Text::new("storage.queue.skipped.halted").text("reason", reason),
                "not_started",
            );
        }
    }

    fn set_status(&mut self, key: &MoveKey, status: MoveStatus) {
        if let Some(job) = self
            .jobs
            .iter_mut()
            .find(|job| &job.key == key && matches!(job.status, MoveStatus::Moving(_)))
        {
            job.status = status;
        }
    }

    fn skip_queued(&mut self, reason: &Text, outcome: &str) {
        for job in self
            .jobs
            .iter_mut()
            .filter(|job| matches!(job.status, MoveStatus::Queued))
        {
            tracing::info!(account_id = job.key.account_id(), game_id = %job.key.game_id(), from = %job.from.display(), to = %job.to.display(), outcome, "move not started");
            job.status = MoveStatus::Skipped(reason.clone());
        }
    }

    /// Moves a waiting job one place among the waiting.
    fn step(&mut self, key: &MoveKey, step: Step) {
        let waiting: Vec<usize> = self
            .jobs
            .iter()
            .enumerate()
            .filter(|(_, job)| matches!(job.status, MoveStatus::Queued))
            .map(|(index, _)| index)
            .collect();
        let Some(at) = waiting
            .iter()
            .position(|&index| self.jobs.get(index).is_some_and(|job| &job.key == key))
        else {
            return;
        };
        let other = match step {
            Step::Up => at.checked_sub(1),
            Step::Down => at.checked_add(1),
        };
        if let (Some(&from), Some(&with)) = (waiting.get(at), other.and_then(|other| waiting.get(other))) {
            self.jobs.swap(from, with);
        }
    }

    fn cancel(&mut self, key: &MoveKey) {
        self.jobs.retain(|job| {
            let drop = &job.key == key && matches!(job.status, MoveStatus::Queued);
            if drop {
                tracing::info!(account_id = job.key.account_id(), game_id = %job.key.game_id(), from = %job.from.display(), to = %job.to.display(), outcome = "cancelled", "move taken out of the queue");
            }
            !drop
        });
    }
}

/// Bytes `item` needs in `to`: none when it is on the same disk, since the
/// transfer is then a rename.
fn room_needed(item: &Movable, to: &Path) -> u64 {
    if same_disk(&item.from, to) { 0 } else { item.size }
}

#[cfg(unix)]
fn same_disk(left: &Path, right: &Path) -> bool {
    use std::os::unix::fs::MetadataExt as _;
    matches!((std::fs::metadata(left), std::fs::metadata(right)), (Ok(left), Ok(right)) if left.dev() == right.dev())
}

/// Without a device id to compare, every move counts as a copy, so the room
/// check errs on asking for too much rather than too little.
#[cfg(not(unix))]
fn same_disk(_: &Path, _: &Path) -> bool {
    false
}

/// Why nothing more can land in `to`: it's gone (an unplugged disk), or it
/// has less room than the move in hand needs.
fn destination_problem(to: &Path, needed: u64) -> Option<Text> {
    if !to.is_dir() {
        return Some(Text::new("storage.queue.errors.missing").arg("path", to.display()));
    }
    let (free, _) = decanter_core::disk_usage(to)?;
    (needed > free).then(|| {
        Text::new("storage.queue.errors.full")
            .arg("path", to.display())
            .arg("needed", crate::format::bytes(needed))
            .arg("free", crate::format::bytes(free))
    })
}

impl StorageView {
    /// Queues `items` to move into `to`, and starts the queue if it was idle.
    fn enqueue_moves(&mut self, items: &[Movable], to: &Path, cx: &mut Context<Self>) {
        match self.moves.enqueue(items, to) {
            Ok(()) => self.error = None,
            Err(err) => self.error = Some(err),
        }
        self.pump_moves(cx);
        cx.notify();
    }

    fn pump_moves(&mut self, cx: &mut Context<Self>) {
        while let Some(turn) = self.moves.take_turn() {
            tracing::info!(account_id = turn.key.account_id(), game_id = %turn.key.game_id(), from = %turn.from.display(), to = %turn.to.display(), outcome = "started", "queued move started");
            if let Some(reason) = destination_problem(&turn.to, turn.needed) {
                tracing::info!(account_id = turn.key.account_id(), game_id = %turn.key.game_id(), from = %turn.from.display(), to = %turn.to.display(), outcome = "halted", %reason, "queued move ended");
                self.moves.finish(
                    &turn,
                    Outcome::Halted {
                        err: reason.clone(),
                        reason,
                    },
                );
                continue;
            }
            let work = Self::start_move(&turn, cx);
            self.tick_while_moving(cx);
            self._move_queue = Some(cx.spawn(async move |this, cx| {
                let result = work.await;
                let _ = this.update(cx, |this, cx| {
                    let outcome = match result {
                        Ok(()) => Outcome::Moved,
                        // Checked again after the fact: a disk filling or going
                        // away mid-copy fails this move with whatever the copy
                        // said, and would fail every move after it the same way.
                        Err(err) => match destination_problem(&turn.to, turn.needed) {
                            Some(reason) => Outcome::Halted { err, reason },
                            None => Outcome::Failed(err),
                        },
                    };
                    let label = match &outcome {
                        Outcome::Moved => "moved",
                        Outcome::Failed(_) => "failed",
                        Outcome::Halted { .. } => "halted",
                    };
                    tracing::info!(account_id = turn.key.account_id(), game_id = %turn.key.game_id(), from = %turn.from.display(), to = %turn.to.display(), outcome = label, "queued move ended");
                    this.moves.finish(&turn, outcome);
                    this.folders_changed(cx);
                    this.pump_moves(cx);
                });
            }));
            return;
        }
    }

    /// The move itself: a game on a blocking thread, a dump through the PS
    /// section, which refuses one running and keeps it from starting meanwhile.
    fn start_move(turn: &Turn, cx: &mut Context<Self>) -> Task<Result<(), Text>> {
        let (to, progress) = (turn.to.clone(), Arc::clone(&turn.progress));
        match &turn.key {
            MoveKey::Game { account_id, game_id } => {
                let (account_id, game_id) = (account_id.clone(), game_id.clone());
                cx.spawn(async move |_, _| {
                    tokio::task::spawn_blocking(move || {
                        crate::relocate::move_game(&account_id, &game_id, &to, &progress)
                    })
                    .await
                    .unwrap_or_else(|err| Err(Text::verbatim(err)))
                })
            }
            MoveKey::Dump(dir) => {
                let dir = dir.clone();
                Ps::get(cx).update(cx, |ps, cx| ps.move_dump(dir, to, progress, cx))
            }
        }
    }

    pub(super) fn moves_moving(&self) -> bool {
        self.moves.is_moving()
    }

    /// The checkbox picking `game` for a group move; none for one that moves
    /// with its launcher.
    pub(super) fn game_select_box(&self, id: usize, game: &InstalledGame, cx: &Context<Self>) -> Option<AnyElement> {
        (game.prefix_of.is_none()).then(|| self.select_box(id, MoveKey::of(game), cx))
    }

    pub(super) fn dump_select_box(&self, id: usize, dump: &Dump, cx: &Context<Self>) -> AnyElement {
        self.select_box(id, MoveKey::Dump(dump.dir.clone()), cx)
    }

    /// Greyed out while its move waits or runs.
    fn select_box(&self, id: usize, key: MoveKey, cx: &Context<Self>) -> AnyElement {
        let queued = self.moves.live(&key).is_some();
        let checked = !queued && self.moves.selected.contains(&key);
        checkbox(checked)
            .id(("storage-select", id))
            .when(queued, |b| b.opacity(0.4))
            .when(!queued, |b| {
                b.cursor_pointer().on_click(cx.listener(move |this, _, _, cx| {
                    if !this.moves.selected.remove(&key) {
                        this.moves.selected.insert(key.clone());
                    }
                    cx.notify();
                }))
            })
            .into_any_element()
    }

    /// "Mover a…" for one game, which queues it on its own.
    pub(super) fn game_move_control(
        &self,
        id: usize,
        game: &InstalledGame,
        from: &Path,
        cx: &Context<Self>,
    ) -> Option<AnyElement> {
        let item = Movable::game(game, from)?;
        self.queued_move_status(&item.key)
            .or_else(|| self.move_control(id, from, item, cx))
    }

    pub(super) fn dump_move_control(
        &self,
        id: usize,
        dump: &Dump,
        from: &Path,
        cx: &Context<Self>,
    ) -> Option<AnyElement> {
        let item = Movable::dump(dump, from);
        self.queued_move_status(&item.key)
            .or_else(|| self.move_control(id, from, item, cx))
    }

    /// The folders `item` can move to, the one it is in left out.
    fn move_control(&self, id: usize, from: &Path, item: Movable, cx: &Context<Self>) -> Option<AnyElement> {
        let targets: Vec<SharedString> = self
            .folders
            .iter()
            .flatten()
            .filter(|folder| folder.present && folder.root != from)
            .map(|folder| SharedString::from(folder.root.to_string_lossy().into_owned()))
            .collect();
        if targets.is_empty() {
            return None;
        }
        let options = std::iter::once((SharedString::default(), t!("storage.move_to").into()))
            .chain(targets.into_iter().map(|path| (path.clone(), path)));
        let this = cx.entity().downgrade();
        let item = [item];
        Some(
            Select::new(("storage-move", id), options, SharedString::default())
                .on_change(move |value, _, cx| {
                    if value.is_empty() {
                        return;
                    }
                    let to = PathBuf::from(value.as_ref());
                    let _ = this.update(cx, |this, cx| this.enqueue_moves(&item, &to, cx));
                })
                .into_any_element(),
        )
    }

    /// Where an item in the queue stands, in place of its own "Mover a…".
    fn queued_move_status(&self, key: &MoveKey) -> Option<AnyElement> {
        let job = self.moves.live(key)?;
        let text = match &job.status {
            MoveStatus::Moving(progress) => moving_text(progress),
            MoveStatus::Queued | MoveStatus::Moved | MoveStatus::Skipped(_) | MoveStatus::Failed(_) => t!(
                "storage.queue.position",
                position = self.moves.queued_position(key).unwrap_or(0).saturating_add(1)
            )
            .into_owned(),
        };
        Some(theme::muted(text).text_size(px(12.)).into_any_element())
    }

    /// The games and dumps picked with their checkboxes that are still listed.
    fn selected_items(&self) -> Vec<Movable> {
        self.folders
            .iter()
            .flatten()
            .flat_map(|folder| {
                folder
                    .games
                    .iter()
                    .filter_map(|game| Movable::game(game, &folder.root))
                    .chain(folder.dumps.iter().map(|dump| Movable::dump(dump, &folder.root)))
            })
            .filter(|item| self.moves.selected.contains(&item.key))
            .collect()
    }

    /// "N juegos seleccionados" with "Mover los seleccionados a…": every
    /// folder that isn't where all of them already are.
    pub(super) fn selection_bar(&self, cx: &Context<Self>) -> Option<impl IntoElement + use<>> {
        let items = self.selected_items();
        if items.is_empty() {
            return None;
        }
        let count = items.len();
        let label = if count == 1 {
            t!("storage.select.count.one")
        } else {
            t!("storage.select.count.other", count = count)
        };
        let options = std::iter::once((SharedString::default(), t!("storage.select.move_to").into())).chain(
            self.folders
                .iter()
                .flatten()
                .filter(|folder| folder.present && items.iter().any(|item| item.from != folder.root))
                .map(|folder| {
                    let path = SharedString::from(folder.root.to_string_lossy().into_owned());
                    (path.clone(), path)
                }),
        );
        let this = cx.entity().downgrade();
        let picker =
            Select::new("storage-move-selected", options, SharedString::default()).on_change(move |value, _, cx| {
                if value.is_empty() {
                    return;
                }
                let to = PathBuf::from(value.as_ref());
                let _ = this.update(cx, |this, cx| {
                    let items = this.selected_items();
                    this.enqueue_moves(&items, &to, cx);
                });
            });
        Some(
            theme::card().mb_4().child(reveal(
                div()
                    .id("storage-selection")
                    .flex()
                    .items_center()
                    .gap_4()
                    .px(px(18.))
                    .py(px(10.))
                    .child(div().flex_1().child(label))
                    .child(picker)
                    .child(
                        button(
                            "storage-select-clear",
                            ButtonKind::Secondary,
                            t!("storage.select.clear"),
                            false,
                        )
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.moves.selected.clear();
                            cx.notify();
                        })),
                    ),
            )),
        )
    }

    /// The queue and how each move in it went: "Moviendo 2 de 5" while it
    /// runs, the tally once it's over.
    pub(super) fn queue_card(&self, cx: &Context<Self>) -> Option<impl IntoElement + use<>> {
        let jobs = &self.moves.jobs;
        if jobs.is_empty() {
            return None;
        }
        let count = |pick: fn(&MoveStatus) -> bool| jobs.iter().filter(|job| pick(&job.status)).count();
        let skipped = count(|status| matches!(status, MoveStatus::Skipped(_)));
        let summary = if self.moves.is_busy() {
            let current = count(|status| matches!(status, MoveStatus::Moved | MoveStatus::Failed(_))).saturating_add(1);
            t!(
                "storage.queue.running",
                current = current,
                total = jobs.len().saturating_sub(skipped),
                skipped = skipped
            )
        } else {
            t!(
                "storage.queue.over",
                moved = count(|status| matches!(status, MoveStatus::Moved)),
                failed = count(|status| matches!(status, MoveStatus::Failed(_))),
                skipped = skipped
            )
        };
        let waiting = count(|status| matches!(status, MoveStatus::Queued));
        let header = reveal(
            div()
                .id("storage-queue-head")
                .flex()
                .items_center()
                .gap_4()
                .px(px(18.))
                .py(px(10.))
                .border_b_1()
                .border_color(gpui::rgb(theme::LINE))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .flex_1()
                        .child(t!("storage.queue.title"))
                        .child(theme::muted(summary).text_size(px(12.))),
                )
                .when(waiting > 0, |row| {
                    row.child(
                        button(
                            "storage-queue-stop",
                            ButtonKind::Secondary,
                            t!("storage.queue.stop"),
                            false,
                        )
                        .tooltip(tooltip(t!("storage.queue.stop_hint")))
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.moves
                                .skip_queued(&Text::new("storage.queue.skipped.stopped"), "stopped");
                            cx.notify();
                        })),
                    )
                })
                .when(!self.moves.is_busy(), |row| {
                    row.child(
                        button(
                            "storage-queue-clear",
                            ButtonKind::Secondary,
                            t!("storage.queue.clear"),
                            false,
                        )
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.moves.jobs.clear();
                            cx.notify();
                        })),
                    )
                }),
        );
        let total = jobs.len();
        let rows = jobs.iter().enumerate().map(|(index, job)| {
            let position = self.moves.queued_position(&job.key);
            Self::queue_row(
                index,
                index.saturating_add(1) == total,
                job,
                position.map(|at| (at, waiting)),
                cx,
            )
        });
        Some(theme::card().mb_4().child(header).children(rows))
    }

    /// One move; `queued` is its place among the `count` waiting, which lets
    /// it be moved up or down or taken out.
    fn queue_row(
        index: usize,
        is_last: bool,
        job: &MoveJob,
        queued: Option<(usize, usize)>,
        cx: &Context<Self>,
    ) -> impl IntoElement + use<> {
        let status: AnyElement = match &job.status {
            MoveStatus::Queued => theme::muted(t!(
                "storage.queue.position",
                position = queued.map_or(1, |(at, _)| at.saturating_add(1))
            ))
            .text_size(px(12.))
            .into_any_element(),
            MoveStatus::Moving(progress) => div()
                .flex()
                .flex_col()
                .gap_1()
                .child(progress_bar(
                    ("storage-queue-bar", index),
                    progress.snapshot().and_then(|(done, total)| fraction(done, total)),
                    4.,
                ))
                .child(theme::muted(moving_text(progress)).text_size(px(12.)))
                .into_any_element(),
            MoveStatus::Moved => theme::muted(t!("storage.queue.moved"))
                .text_size(px(12.))
                .into_any_element(),
            MoveStatus::Skipped(reason) => theme::muted(reason).text_size(px(12.)).into_any_element(),
            MoveStatus::Failed(err) => theme::error(err).text_size(px(12.)).into_any_element(),
        };
        let buttons = queued.map(|(at, waiting)| {
            let step = |id: &'static str, glyph: &'static str, hint, to: Step, disabled: bool| {
                let key = job.key.clone();
                button((id, index), ButtonKind::Secondary, glyph, disabled)
                    .tooltip(tooltip(hint))
                    .when(!disabled, |b| {
                        b.on_click(cx.listener(move |this, _, _, cx| {
                            this.moves.step(&key, to);
                            cx.notify();
                        }))
                    })
            };
            let key = job.key.clone();
            div()
                .flex()
                .flex_none()
                .gap_2()
                .child(step(
                    "storage-queue-up",
                    "↑",
                    t!("downloads.move_up"),
                    Step::Up,
                    at == 0,
                ))
                .child(step(
                    "storage-queue-down",
                    "↓",
                    t!("downloads.move_down"),
                    Step::Down,
                    at.saturating_add(1) == waiting,
                ))
                .child(
                    button(
                        ("storage-queue-cancel", index),
                        ButtonKind::Icon,
                        t!("common.cancel"),
                        false,
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.moves.cancel(&key);
                        cx.notify();
                    })),
                )
        });
        reveal(
            theme::list_row(("storage-queue-row", index), is_last)
                .items_start()
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .flex_1()
                        .min_w_0()
                        .gap_1()
                        .child(job.title.clone())
                        .child(
                            theme::muted(t!(
                                "storage.queue.route",
                                from = job.from.display(),
                                to = job.to.display()
                            ))
                            .text_size(px(11.))
                            .truncate(),
                        )
                        .child(status),
                )
                .children(buttons),
        )
    }
}

/// `done` of `total` as the bar draws it, to a thousandth.
fn fraction(done: u64, total: u64) -> Option<f64> {
    let permille = done.saturating_mul(1000).checked_div(total)?.min(1000);
    u32::try_from(permille)
        .ok()
        .map(|permille| f64::from(permille) / 1000.0)
}

fn moving_text(progress: &Progress) -> String {
    progress
        .snapshot()
        .map_or_else(
            || t!("storage.moving.started"),
            |(done, total)| {
                t!(
                    "storage.moving.progress",
                    done = crate::format::bytes(done),
                    total = crate::format::bytes(total)
                )
            },
        )
        .into_owned()
}
