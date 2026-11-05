//! Moving games and PS dumps between library folders, several in one go: a
//! queue run the way Descargas runs downloads (#1349), one at a time in the
//! order shown, reorderable while it waits. A game goes through
//! [`crate::relocate::move_game`], a dump through [`Ps::move_dump`], as a
//! single move of either always does. A destination that is gone or full
//! stops the moves still waiting, which stay where they are.
//!
//! It is shared state so it keeps running when Almacenamiento is left, and so
//! a game's menu queues into the same list that page shows.

use std::borrow::Cow;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use decanter_core::InstalledGame;
use gpui::{Context, Entity, EventEmitter, Task};
use rust_i18n::t;

use super::{GogLibrary, Ps};
use crate::external::transfer::Progress;
use crate::i18n::Text;

/// One move per game and account, as the library files a game per account,
/// or per PS dump, by the folder it is in.
#[derive(Clone, PartialEq, Eq, Hash)]
pub enum MoveKey {
    Game { account_id: String, game_id: String },
    Dump(PathBuf),
}

impl MoveKey {
    pub fn of(game: &InstalledGame) -> Self {
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
#[derive(Clone)]
pub struct Movable {
    pub key: MoveKey,
    pub title: String,
    pub from: PathBuf,
    pub size: u64,
}

impl Movable {
    /// `None` for a game a launcher installed, which moves with its launcher.
    pub fn game(game: &InstalledGame, from: &Path) -> Option<Self> {
        game.prefix_of.is_none().then(|| Self {
            key: MoveKey::of(game),
            title: game.name.clone(),
            from: from.to_owned(),
            size: game.size_on_disk.unwrap_or(0),
        })
    }

    pub fn dump(dir: &Path, name: &str, size: u64, from: &Path) -> Self {
        Self {
            key: MoveKey::Dump(dir.to_owned()),
            title: name.to_owned(),
            from: from.to_owned(),
            size,
        }
    }
}

pub enum MoveStatus {
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

pub struct MoveJob {
    pub key: MoveKey,
    pub title: String,
    pub from: PathBuf,
    pub to: PathBuf,
    /// What has to fit in `to`: nothing for a move within one disk, which
    /// is a rename.
    pub needed: u64,
    pub status: MoveStatus,
}

#[derive(Clone, Copy)]
pub enum Step {
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

/// A move ended, so the folders' contents are not what they were.
pub struct MoveFinished;

pub struct MoveQueue {
    /// In the order they run; finished ones stay until cleared, as the
    /// per-game result.
    jobs: Vec<MoveJob>,
    /// Why the last move asked for from a game's menu was turned down, kept
    /// for Almacenamiento to show since the menu has nowhere to.
    refused: Option<Text>,
    library: Entity<GogLibrary>,
    work: Option<Task<()>>,
    /// Redraws the progress while a move runs: it runs on a blocking
    /// thread that can't notify this entity itself.
    ticker: Option<Task<()>>,
}

impl EventEmitter<MoveFinished> for MoveQueue {}

impl MoveQueue {
    pub fn new(library: Entity<GogLibrary>) -> Self {
        Self {
            jobs: Vec::new(),
            refused: None,
            library,
            work: None,
            ticker: None,
        }
    }

    pub fn jobs(&self) -> &[MoveJob] {
        &self.jobs
    }

    pub fn clear(&mut self) {
        self.jobs.clear();
    }

    pub fn is_busy(&self) -> bool {
        self.jobs.iter().any(|job| job.status.is_live())
    }

    fn is_moving(&self) -> bool {
        self.jobs.iter().any(|job| matches!(job.status, MoveStatus::Moving(_)))
    }

    pub fn live(&self, key: &MoveKey) -> Option<&MoveJob> {
        self.jobs.iter().find(|job| &job.key == key && job.status.is_live())
    }

    pub fn queued_position(&self, key: &MoveKey) -> Option<usize> {
        self.jobs
            .iter()
            .filter(|job| matches!(job.status, MoveStatus::Queued))
            .position(|job| &job.key == key)
    }

    pub fn take_refused(&mut self) -> Option<Text> {
        self.refused.take()
    }

    /// Queues `items` to move into `to`, and starts the queue if it was idle.
    /// Refused as a whole when what has to be copied there, with what is
    /// already queued for it, clearly won't fit.
    pub fn enqueue(&mut self, items: &[Movable], to: &Path, cx: &mut Context<Self>) -> Result<(), Text> {
        let result = self.add_jobs(items, to);
        self.pump(cx);
        cx.notify();
        result
    }

    /// [`Self::enqueue`] for a request with no page of its own to show a
    /// refusal on.
    pub fn enqueue_quietly(&mut self, items: &[Movable], to: &Path, cx: &mut Context<Self>) {
        self.refused = self.enqueue(items, to, cx).err();
    }

    fn add_jobs(&mut self, items: &[Movable], to: &Path) -> Result<(), Text> {
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

    pub fn skip_queued(&mut self, reason: &Text, outcome: &str) {
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
    pub fn step(&mut self, key: &MoveKey, step: Step) {
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

    pub fn cancel(&mut self, key: &MoveKey) {
        self.jobs.retain(|job| {
            let drop = &job.key == key && matches!(job.status, MoveStatus::Queued);
            if drop {
                tracing::info!(account_id = job.key.account_id(), game_id = %job.key.game_id(), from = %job.from.display(), to = %job.to.display(), outcome = "cancelled", "move taken out of the queue");
            }
            !drop
        });
    }

    fn pump(&mut self, cx: &mut Context<Self>) {
        while let Some(turn) = self.take_turn() {
            tracing::info!(account_id = turn.key.account_id(), game_id = %turn.key.game_id(), from = %turn.from.display(), to = %turn.to.display(), outcome = "started", "queued move started");
            if let Some(reason) = destination_problem(&turn.to, turn.needed) {
                tracing::info!(account_id = turn.key.account_id(), game_id = %turn.key.game_id(), from = %turn.from.display(), to = %turn.to.display(), outcome = "halted", %reason, "queued move ended");
                self.finish(
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
            self.work = Some(cx.spawn(async move |this, cx| {
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
                    this.finish(&turn, outcome);
                    // What changed is which disks the library reads from, so
                    // the library re-reads what's installed.
                    this.library
                        .clone()
                        .update(cx, |library, cx| library.refresh_all_install_state(cx));
                    cx.emit(MoveFinished);
                    cx.notify();
                    this.pump(cx);
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

    fn tick_while_moving(&mut self, cx: &Context<Self>) {
        self.ticker = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_millis(250)).await;
                let still = this.update(cx, |this, cx| {
                    cx.notify();
                    this.is_moving()
                });
                if !matches!(still, Ok(true)) {
                    return;
                }
            }
        }));
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

/// The library folders of `profile_id` that are on a disk right now: the
/// ones a move can land in.
pub fn present_folders(profile_id: &str) -> Vec<PathBuf> {
    let primary = crate::core::library_for_profile(profile_id)
        .inspect_err(|err| tracing::warn!(profile_id, %err, "failed to resolve library root for moves"))
        .ok()
        .map(|library| library.root().to_path_buf());
    let extra = crate::core::db()
        .ok()
        .and_then(|db| db.profile_extra_libraries(profile_id).ok())
        .unwrap_or_default()
        .into_iter()
        .map(PathBuf::from);
    primary.into_iter().chain(extra).filter(|root| root.is_dir()).collect()
}

/// `done` of `total` as the bar draws it, to a thousandth.
pub fn fraction(done: u64, total: u64) -> Option<f64> {
    let permille = done.saturating_mul(1000).checked_div(total)?.min(1000);
    u32::try_from(permille)
        .ok()
        .map(|permille| f64::from(permille) / 1000.0)
}

pub fn moving_text(progress: &Progress) -> String {
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
