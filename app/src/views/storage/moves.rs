//! Almacenamiento's side of moving games and PS dumps: picking them, and the
//! queue card. The queue itself is [`crate::state::MoveQueue`].

use std::path::{Path, PathBuf};

use decanter_core::InstalledGame;
use gpui::{AnyElement, Context, SharedString, div, prelude::*, px};
use rust_i18n::t;

use super::{Dump, StorageView};
use crate::i18n::Text;
use crate::state::{Movable, MoveJob, MoveKey, MoveStatus, MoveStep as Step, move_fraction, moving_text};
use crate::theme;
use crate::widgets::reveal::reveal;
use crate::widgets::select::Select;
use crate::widgets::{ButtonKind, button, checkbox, progress_bar, tooltip};

impl Dump {
    fn movable(&self, from: &Path) -> Movable {
        Movable::dump(&self.dir, &self.name, self.size, from)
    }
}

impl StorageView {
    /// Queues `items` to move into `to`, and starts the queue if it was idle.
    fn enqueue_moves(&mut self, items: &[Movable], to: &Path, cx: &mut Context<Self>) {
        match self.queue.update(cx, |queue, cx| queue.enqueue(items, to, cx)) {
            Ok(()) => {
                self.error = None;
                self.selected.clear();
            }
            Err(err) => self.error = Some(err),
        }
        cx.notify();
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
        let queued = self.queue.read(cx).live(&key).is_some();
        let checked = !queued && self.selected.contains(&key);
        checkbox(checked)
            .id(("storage-select", id))
            .when(queued, |b| b.opacity(0.4))
            .when(!queued, |b| {
                b.cursor_pointer().on_click(cx.listener(move |this, _, _, cx| {
                    if !this.selected.remove(&key) {
                        this.selected.insert(key.clone());
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
        self.queued_move_status(&item.key, cx)
            .or_else(|| self.move_control(id, from, item, cx))
    }

    pub(super) fn dump_move_control(
        &self,
        id: usize,
        dump: &Dump,
        from: &Path,
        cx: &Context<Self>,
    ) -> Option<AnyElement> {
        let item = dump.movable(from);
        self.queued_move_status(&item.key, cx)
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
    fn queued_move_status(&self, key: &MoveKey, cx: &Context<Self>) -> Option<AnyElement> {
        let queue = self.queue.read(cx);
        let job = queue.live(key)?;
        let text = match &job.status {
            MoveStatus::Moving(progress) => moving_text(progress),
            MoveStatus::Queued | MoveStatus::Moved | MoveStatus::Skipped(_) | MoveStatus::Failed(_) => t!(
                "storage.queue.position",
                position = queue.queued_position(key).unwrap_or(0).saturating_add(1)
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
                    .chain(folder.dumps.iter().map(|dump| dump.movable(&folder.root)))
            })
            .filter(|item| self.selected.contains(&item.key))
            .collect()
    }

    /// "N juegos seleccionados" with "Mover los seleccionados a…": every
    /// folder that isn't where all of them already are.
    pub(super) fn selection_bar(&self, cx: &Context<Self>) -> Option<impl IntoElement + use<>> {
        if self.selected.is_empty() {
            return None;
        }
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
                            this.selected.clear();
                            cx.notify();
                        })),
                    ),
            )),
        )
    }

    /// The queue and how each move in it went: "Moviendo 2 de 5" while it
    /// runs, the tally once it's over.
    pub(super) fn queue_card(&self, cx: &Context<Self>) -> Option<impl IntoElement + use<>> {
        let queue = self.queue.read(cx);
        let jobs = queue.jobs();
        if jobs.is_empty() {
            return None;
        }
        let count = |pick: fn(&MoveStatus) -> bool| jobs.iter().filter(|job| pick(&job.status)).count();
        let skipped = count(|status| matches!(status, MoveStatus::Skipped(_)));
        let summary = if queue.is_busy() {
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
                            this.queue.update(cx, |queue, cx| {
                                queue.skip_queued(&Text::new("storage.queue.skipped.stopped"), "stopped");
                                cx.notify();
                            });
                        })),
                    )
                })
                .when(!queue.is_busy(), |row| {
                    row.child(
                        button(
                            "storage-queue-clear",
                            ButtonKind::Secondary,
                            t!("storage.queue.clear"),
                            false,
                        )
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.queue.update(cx, |queue, cx| {
                                queue.clear();
                                cx.notify();
                            });
                        })),
                    )
                }),
        );
        let total = jobs.len();
        let rows = jobs.iter().enumerate().map(|(index, job)| {
            let position = queue.queued_position(&job.key);
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
                    progress.snapshot().and_then(|(done, total)| move_fraction(done, total)),
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
                            this.queue.update(cx, |queue, cx| {
                                queue.step(&key, to);
                                cx.notify();
                            });
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
                        this.queue.update(cx, |queue, cx| {
                            queue.cancel(&key);
                            cx.notify();
                        });
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
