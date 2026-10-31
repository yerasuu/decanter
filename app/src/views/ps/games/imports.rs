//! Imports: what the game's last run says psport does not cover, read from its run log so it shows
//! without debug mode, and the loader's decisions about the system modules it serves itself.

use std::borrow::Cow;
use std::io;
use std::path::PathBuf;

use decanter_ps::imports::{self, Decision, Gap, Import, ImportReport};
use decanter_ps::process::{Job, JobKind};
use decanter_ps::run_log;
use gpui::{AnyElement, Context, Entity, Window, div, prelude::*, px, rgb};
use rust_i18n::t;

use crate::i18n::Text;
use crate::state::{JobKey, Ps, job_target};
use crate::theme;
use crate::widgets::stat_card;

const GAPS: [Gap; 4] = [Gap::Unimplemented, Gap::Stub, Gap::LeftToHost, Gap::Called];
const MIB: u64 = 1 << 20;

enum Loaded {
    Reading,
    NoRun,
    Run(ImportReport),
}

pub struct ImportsView {
    ps: Entity<Ps>,
    dir: PathBuf,
    title_id: String,
    loaded: Loaded,
    error: Option<Text>,
    /// `None` shows every kind.
    filter: Option<Gap>,
    /// The game's job last seen and whether it ran: the log is read again once it has ended.
    seen: Option<(Entity<Job>, bool)>,
}

impl ImportsView {
    pub fn new(ps: Entity<Ps>, dir: PathBuf, title_id: String, cx: &mut Context<Self>) -> Self {
        cx.observe(&ps, |this, _, cx| this.sync_job(cx)).detach();
        let mut this = Self {
            ps,
            dir,
            title_id,
            loaded: Loaded::Reading,
            error: None,
            filter: None,
            seen: None,
        };
        this.seen = this.current_job(cx);
        this.reload(cx);
        this
    }

    fn current_job(&self, cx: &Context<Self>) -> Option<(Entity<Job>, bool)> {
        let job = self.ps.read(cx).job(&JobKey::Game(self.dir.clone()))?.clone();
        let running = job.read(cx).is_running();
        Some((job, running))
    }

    fn sync_job(&mut self, cx: &mut Context<Self>) {
        let now = self.current_job(cx);
        if self.seen == now {
            return;
        }
        let ended = now.as_ref().is_some_and(|(_, running)| !running);
        self.seen = now;
        if ended {
            self.reload(cx);
        }
    }

    fn reload(&self, cx: &mut Context<Self>) {
        let target = job_target(&JobKey::Game(self.dir.clone()));
        let task = cx.background_spawn(async move { newest_report(&target) });
        cx.spawn(async move |this, cx| {
            let read = task.await;
            let _ = this.update(cx, |this, cx| {
                match read {
                    Ok(report) => {
                        this.error = None;
                        this.filter = None;
                        this.loaded = report.map_or(Loaded::NoRun, Loaded::Run);
                    }
                    Err(err) => {
                        tracing::warn!(title_id = %this.title_id, %err, "could not read the game's last run log");
                        this.error = Some(Text::new("ps.imports.read_error").arg("error", err));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn report(&self, report: &ImportReport, cx: &Context<Self>) -> AnyElement {
        let started = report.started.as_deref().unwrap_or("—");
        let header = div()
            .flex()
            .items_baseline()
            .gap_3()
            .child(theme::muted(t!("ps.imports.run")))
            .child(
                theme::mono()
                    .text_color(rgb(theme::TEXT_PRIMARY))
                    .child(started.to_owned()),
            );
        let Some(totals) = report.totals else {
            return div()
                .flex()
                .flex_col()
                .gap_3()
                .child(header)
                .child(theme::empty(t!("ps.imports.old_port")))
                .into_any_element();
        };
        let cards = div()
            .flex()
            .flex_wrap()
            .gap_2()
            .child(stat_card(totals.imports.to_string(), &t!("ps.imports.totals.imports")))
            .child(stat_card(totals.host.to_string(), &t!("ps.imports.totals.host")))
            .child(stat_card(
                totals.game_modules.to_string(),
                &t!("ps.imports.totals.game_modules"),
            ))
            .child(stat_card(
                totals.system_modules.to_string(),
                &t!("ps.imports.totals.system_modules"),
            ))
            .when(totals.loaded_modules > 0, |cards| {
                cards.child(stat_card(
                    totals.loaded_modules.to_string(),
                    &t!("ps.imports.totals.loaded_modules"),
                ))
            })
            .child(stat_card(totals.stubs.to_string(), &t!("ps.imports.totals.stubs")))
            .child(stat_card(
                totals.unimplemented.to_string(),
                &t!("ps.imports.totals.unimplemented"),
            ));
        let notes = [
            (report.unread_bytes > 0).then(|| {
                t!(
                    "ps.imports.truncated",
                    read = imports::MAX_LOG_BYTES.div_ceil(MIB),
                    unread = report.unread_bytes.div_ceil(MIB)
                )
            }),
            (report.dropped > 0).then(|| t!("ps.imports.dropped", count = report.dropped)),
        ];
        div()
            .flex()
            .flex_col()
            .gap_3()
            .child(header)
            .child(cards)
            .children(
                notes
                    .into_iter()
                    .flatten()
                    .map(|note| theme::muted(note).text_size(px(12.))),
            )
            .child(div().mt_4().child(theme::h3(t!("ps.imports.title"))))
            .child(self.imports(&report.imports, cx))
            .children((!report.decisions.is_empty()).then(|| {
                div()
                    .mt_4()
                    .flex()
                    .flex_col()
                    .child(theme::h3(t!("ps.imports.decisions")))
                    .child(decisions(&report.decisions))
            }))
            .into_any_element()
    }

    fn imports(&self, imports: &[Import], cx: &Context<Self>) -> AnyElement {
        if imports.is_empty() {
            return theme::empty(t!("ps.imports.all_covered")).into_any_element();
        }
        let all = (None, format!("{} ({})", t!("ps.imports.filter.all"), imports.len()));
        let items: Vec<_> = std::iter::once(all)
            .chain(GAPS.iter().filter_map(|&gap| {
                let count = imports.iter().filter(|import| import.gap == gap).count();
                (count > 0).then(|| (Some(gap), format!("{} ({count})", gap_label(gap))))
            }))
            .collect();
        let entity = cx.entity();
        let filter = crate::widgets::tabs("ps-imports-filter", &items, self.filter, move |gap, _, cx| {
            entity.update(cx, |this, cx| {
                this.filter = gap;
                cx.notify();
            });
        });
        let shown: Vec<_> = imports
            .iter()
            .filter(|import| self.filter.is_none_or(|gap| import.gap == gap))
            .collect();
        let last = shown.len().saturating_sub(1);
        let column = |text: Cow<'static, str>| {
            div()
                .text_size(px(10.5))
                .text_color(rgb(theme::TEXT_MUTED))
                .child(text.to_uppercase())
        };
        let header = theme::compact_row(false)
            .child(column(t!("ps.imports.column.nid")).w(px(120.)))
            .child(column(t!("ps.imports.column.name")).flex_1())
            .child(column(t!("ps.imports.column.kind")).w(px(200.)))
            .child(column(t!("ps.imports.column.calls")).w(px(80.)));
        let rows = shown.into_iter().enumerate().map(|(index, import)| {
            let name = import.name.as_ref().map_or_else(
                || theme::muted(t!("ps.imports.unresolved")).italic(),
                |name| theme::mono().child(name.clone()),
            );
            let calls = match (import.calls, import.called) {
                (Some(count), _) => count.to_string(),
                (None, true) => t!("ps.imports.called").into_owned(),
                (None, false) => "—".to_owned(),
            };
            theme::compact_row(index == last)
                .text_size(px(12.))
                .text_color(rgb(theme::TEXT_PRIMARY))
                .child(theme::mono().w(px(120.)).child(import.nid.clone()))
                .child(name.flex_1().min_w_0().overflow_hidden())
                .child(div().w(px(200.)).child(gap_label(import.gap)))
                .child(theme::mono().w(px(80.)).child(calls))
        });
        div()
            .flex()
            .flex_col()
            .gap_3()
            .child(filter)
            .child(theme::card().child(header).children(rows))
            .child(
                theme::muted(t!("ps.imports.calls_note"))
                    .text_size(px(11.))
                    .text_color(rgb(theme::TEXT_DIM)),
            )
            .into_any_element()
    }
}

fn gap_label(gap: Gap) -> Cow<'static, str> {
    match gap {
        Gap::Unimplemented => t!("ps.imports.gap.unimplemented"),
        Gap::Stub => t!("ps.imports.gap.stub"),
        Gap::LeftToHost => t!("ps.imports.gap.left_to_host"),
        Gap::Called => t!("ps.imports.gap.called"),
    }
}

fn decisions(decisions: &[Decision]) -> impl IntoElement {
    let last = decisions.len().saturating_sub(1);
    theme::card().children(decisions.iter().enumerate().map(|(index, decision)| {
        let (module, verdict, reason) = match decision {
            Decision::LinkedAlthough { module, reason, lacks } => (
                module,
                lacks.as_ref().map_or_else(
                    || t!("ps.imports.linked_although_some"),
                    |lacks| {
                        t!(
                            "ps.imports.linked_although",
                            count = lacks.count,
                            library = lacks.library,
                            first = lacks.first
                        )
                    },
                ),
                reason,
            ),
            Decision::Served { module, reason } => (module, t!("ps.imports.served"), reason),
        };
        theme::compact_row(index == last)
            .items_start()
            .text_size(px(12.))
            .child(
                theme::mono()
                    .w(px(220.))
                    .text_color(rgb(theme::TEXT_PRIMARY))
                    .child(module.clone()),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        div()
                            .text_color(rgb(if matches!(decision, Decision::LinkedAlthough { .. }) {
                                theme::ACCENT_GOLD
                            } else {
                                theme::TEXT_PRIMARY
                            }))
                            .child(verdict),
                    )
                    .child(theme::mono().text_color(rgb(theme::TEXT_MUTED)).child(reason.clone())),
            )
    }))
}

/// The newest kept log of the game's runs, `None` when it has none.
fn newest_report(target: &str) -> io::Result<Option<ImportReport>> {
    let dir = run_log::dir(&decanter_ps::paths::active());
    run_log::kept(&dir, target, JobKind::Game)?
        .first()
        .map(|log| ImportReport::read(log))
        .transpose()
}

impl Render for ImportsView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let body = match &self.loaded {
            Loaded::Reading => theme::muted(t!("ps.imports.reading")).into_any_element(),
            Loaded::NoRun => theme::empty(t!("ps.detail.no_runs")).into_any_element(),
            Loaded::Run(report) => self.report(report, cx),
        };
        div()
            .flex()
            .flex_col()
            .gap_3()
            .children(self.error.as_ref().map(theme::error))
            .child(body)
    }
}
