use std::rc::Rc;

use decanter_gog::VerifyReport;
use gpui::{Context, Window, prelude::*};

use super::{Target, error_line, field_row, not_installed, pane};
use crate::theme;
use crate::widgets::{ButtonKind, button};

pub(super) struct LocalFilesTab {
    target: Rc<Target>,
    installed: bool,
    verifying: bool,
    report: Option<VerifyReport>,
    repairing: bool,
    error: String,
}

impl LocalFilesTab {
    pub(super) fn new(target: Rc<Target>) -> Self {
        let installed = crate::core::installed_game(&target.account_id, &target.product_id).is_some();
        Self {
            target,
            installed,
            verifying: false,
            report: None,
            repairing: false,
            error: String::new(),
        }
    }

    fn reload(&mut self) {
        self.installed = crate::core::installed_game(&self.target.account_id, &self.target.product_id).is_some();
    }

    fn verify(&mut self, cx: &mut Context<Self>) {
        let target = Rc::clone(&self.target);
        self.verifying = true;
        self.error.clear();
        self.report = None;
        tracing::info!(
            account_id = target.account_id,
            product_id = target.product_id,
            "file verification started"
        );
        cx.notify();
        cx.spawn(async move |this, cx| {
            let account_id = target.account_id.as_str();
            let product_id = target.product_id.as_str();
            let Ok(core_library) = crate::core::library_for_account(account_id) else {
                tracing::error!(
                    account_id,
                    product_id,
                    "could not resolve library, verification aborted"
                );
                let _ = this.update(cx, |this, cx| {
                    this.error = "no se pudo resolver la biblioteca".to_string();
                    this.verifying = false;
                    cx.notify();
                });
                return;
            };
            let outcome = decanter_gog::verify(&core_library, account_id, product_id).await;
            let _ = this.update(cx, |this, cx| {
                match outcome {
                    Ok(outcome) => {
                        tracing::info!(
                            account_id,
                            product_id,
                            files_checked = outcome.files_checked,
                            broken = outcome.broken.len(),
                            "file verification finished",
                        );
                        this.report = Some(outcome);
                    }
                    Err(err) => {
                        tracing::error!(account_id, product_id, error = %err, "file verification failed");
                        this.error = err.to_string();
                    }
                }
                this.verifying = false;
                this.reload();
                cx.notify();
            });
        })
        .detach();
    }

    fn repair(&mut self, cx: &mut Context<Self>) {
        let Some(broken) = self.report.as_ref().map(|report| report.broken.clone()) else {
            return;
        };
        let target = Rc::clone(&self.target);
        self.repairing = true;
        self.error.clear();
        tracing::info!(
            account_id = target.account_id,
            product_id = target.product_id,
            broken_count = broken.len(),
            "file repair started"
        );
        cx.notify();
        cx.spawn(async move |this, cx| {
            let account_id = target.account_id.as_str();
            let product_id = target.product_id.as_str();
            let fail = |error: String, cx: &mut gpui::AsyncApp| {
                let _ = this.update(cx, |this, cx| {
                    this.error = error;
                    this.repairing = false;
                    cx.notify();
                });
            };
            let Ok(core_library) = crate::core::library_for_account(account_id) else {
                tracing::error!(account_id, product_id, "could not resolve library, repair aborted");
                fail("no se pudo resolver la biblioteca".to_string(), cx);
                return;
            };
            if let Err(err) = decanter_gog::repair(&core_library, account_id, product_id, &broken).await {
                tracing::error!(account_id, product_id, error = %err, "file repair failed");
                fail(err.to_string(), cx);
                return;
            }
            tracing::info!(account_id, product_id, "file repair finished");
            let outcome = decanter_gog::verify(&core_library, account_id, product_id).await;
            let _ = this.update(cx, |this, cx| {
                match outcome {
                    Ok(outcome) => this.report = Some(outcome),
                    Err(err) => {
                        tracing::error!(account_id, product_id, error = %err, "re-verify after repair failed");
                        this.error = err.to_string();
                    }
                }
                this.repairing = false;
                this.reload();
                cx.notify();
            });
        })
        .detach();
    }
}

impl Render for LocalFilesTab {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !self.installed {
            return not_installed();
        }

        let verify_label = if self.verifying {
            "Verificando..."
        } else {
            "Verificar archivos"
        };
        let mut actions = field_row().child(
            button("verify-files", ButtonKind::Secondary, verify_label, self.verifying).when(!self.verifying, |b| {
                b.on_click(cx.listener(|this, _, _, cx| this.verify(cx)))
            }),
        );
        if let Some(report) = &self.report {
            if report.is_clean() {
                actions = actions.child(theme::muted(format!(
                    "{} archivos, todo en orden.",
                    report.files_checked
                )));
            } else {
                let repair_label = if self.repairing { "Reparando..." } else { "Reparar" };
                actions = actions
                    .child(theme::error(format!(
                        "{} de {} archivos no coinciden.",
                        report.broken.len(),
                        report.files_checked
                    )))
                    .child(
                        button("repair-files", ButtonKind::Primary, repair_label, self.repairing)
                            .when(!self.repairing, |b| {
                                b.on_click(cx.listener(|this, _, _, cx| this.repair(cx)))
                            }),
                    );
            }
        }

        pane()
            .child(theme::muted(
                "Compara cada archivo instalado contra el manifiesto de GOG y vuelve a bajar lo que no coincida.",
            ))
            .child(actions)
            .children(error_line(&self.error))
    }
}
