//! Ports: where each port stands — toolchain, checkout, build, upstream —
//! and its latest clone, build or update output.
//!
//! The toolchain line is information only: which one runs a port is
//! decided by what the machine has.

use decanter_ps::paths;
use decanter_ps::port::Port;
use decanter_ps::updater::Plan;
use gpui::{AnyElement, Context, Entity, Window, div, prelude::*, px, rgb};

use super::log::LogView;
use crate::state::{JobKey, Ps, Stage};
use crate::theme;
use crate::widgets::{ButtonKind, button, progress_bar};

pub struct PortsView {
    ps: Entity<Ps>,
    logs: Vec<(Port, Entity<LogView>)>,
    /// The port whose reset is waiting for its confirming second click.
    confirm_reset: Option<Port>,
}

impl PortsView {
    pub fn new(ps: Entity<Ps>, cx: &mut Context<Self>) -> Self {
        cx.observe(&ps, |this, _, cx| {
            this.sync_logs(cx);
            cx.notify();
        })
        .detach();
        let mut this = Self {
            ps,
            logs: Vec::new(),
            confirm_reset: None,
        };
        this.sync_logs(cx);
        this
    }

    /// A port's log view follows whichever job last ran for it.
    fn sync_logs(&mut self, cx: &mut Context<Self>) {
        for port in Port::ALL {
            let Some(job) = self.ps.read(cx).job(&JobKey::Port(port)).cloned() else {
                continue;
            };
            let current = self.logs.iter().position(|(logged, _)| *logged == port);
            match current {
                Some(index) if self.logs[index].1.read(cx).job() == &job => {}
                Some(index) => self.logs[index].1 = cx.new(|cx| LogView::new(job, cx)),
                None => self.logs.push((port, cx.new(|cx| LogView::new(job, cx)))),
            }
        }
    }

    fn card(&self, port: Port, cx: &mut Context<Self>) -> AnyElement {
        let ps = self.ps.read(cx);
        let state = ps.port(port);
        let stage = state.map_or(&Stage::Idle, |state| state.stage());
        let busy = state.is_some_and(|state| state.is_busy());
        let (stage_text, progress) = match stage {
            Stage::Idle => ("Se prepara la primera vez que haga falta".to_owned(), None),
            Stage::Resolving => ("Buscando herramientas…".to_owned(), None),
            Stage::Downloading(received, total) => (
                format!("Descargando nix-portable: {}", crate::format::bytes(*received)),
                total.map(|total| *received as f64 / total.max(1) as f64),
            ),
            Stage::Cloning => ("Clonando…".to_owned(), None),
            Stage::Updating => ("Actualizando…".to_owned(), None),
            Stage::Building => ("Compilando…".to_owned(), None),
            Stage::Ready => ("Listo".to_owned(), None),
            Stage::Failed(message) => (message.clone(), None),
        };
        let toolchain = state
            .and_then(|state| state.toolchain())
            .map(|toolchain| format!("Entorno: {}", toolchain.label()));
        let check = state.and_then(|state| state.check());
        let upstream = check.map(|check| match check {
            Ok(check) => match &check.plan {
                Plan::UpToDate => format!("{} · al día con {}", check.head, check.branch),
                Plan::FastForward => format!("{} · {} commits nuevos en {}", check.head, check.behind, check.branch),
                Plan::Rewritten => format!("{} · upstream reescribió su historia", check.head),
                Plan::LocalCommits(count) => format!("{} · {count} commits locales que upstream no tiene", check.head),
            },
            Err(err) => format!("No se pudo consultar upstream: {err}"),
        });
        let needs_reset = check.is_some_and(
            |check| matches!(check, Ok(check) if matches!(check.plan, Plan::Rewritten | Plan::LocalCommits(_))),
        );
        let failed = matches!(stage, Stage::Failed(_));
        let confirming = self.confirm_reset == Some(port);
        let log = self
            .logs
            .iter()
            .find(|(logged, _)| *logged == port)
            .map(|(_, log)| log.clone());

        theme::card()
            .p_4()
            .gap_2()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .child(theme::h3(port.name()))
                    .child(theme::muted(port.upstream()).text_size(px(11.))),
            )
            .child(
                div()
                    .text_color(rgb(if failed { theme::ERROR_TEXT } else { theme::TEXT_PRIMARY }))
                    .child(stage_text),
            )
            .children(progress.map(|fraction| progress_bar(("ps-port-progress", port as usize), Some(fraction), 4.)))
            .children(toolchain.map(theme::muted))
            .children(upstream.map(theme::muted))
            .child(
                div()
                    .flex()
                    .gap_2()
                    .mt_2()
                    .child(
                        button(
                            ("ps-port-prepare", port as usize),
                            ButtonKind::Secondary,
                            "Preparar",
                            busy,
                        )
                        .when(!busy, |b| {
                            b.on_click(cx.listener(move |this, _, _, cx| {
                                this.ps
                                    .update(cx, |ps, cx| drop(ps.prepare(port, &paths::active(), cx)));
                            }))
                        }),
                    )
                    .when(needs_reset, |row| {
                        let label = if confirming {
                            "Confirmar: guardar cambios locales y restablecer"
                        } else {
                            "Respaldar y actualizar"
                        };
                        row.child(
                            button(("ps-port-reset", port as usize), ButtonKind::Secondary, label, busy).when(
                                !busy,
                                |b| {
                                    b.on_click(cx.listener(move |this, _, _, cx| {
                                        if this.confirm_reset == Some(port) {
                                            this.confirm_reset = None;
                                            this.ps.update(cx, |ps, cx| ps.reset_to_upstream(port, cx));
                                        } else {
                                            this.confirm_reset = Some(port);
                                        }
                                        cx.notify();
                                    }))
                                },
                            ),
                        )
                    }),
            )
            .children(log.map(|log| div().h(px(220.)).mt_2().child(log)))
            .into_any_element()
    }
}

impl Render for PortsView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let cards: Vec<AnyElement> = Port::ALL.into_iter().map(|port| self.card(port, cx)).collect();
        theme::page(
            "ps-ports",
            div()
                .flex()
                .flex_col()
                .gap_4()
                .child(theme::h2("Ports"))
                .child(theme::muted(
                    "Cada port se clona, se compila y se actualiza solo, con las herramientas del sistema \
                     cuando alcanzan y con nix cuando no.",
                ))
                .children(cards),
        )
    }
}
