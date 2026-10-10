//! Getting a port ready without asking: toolchain, checkout and build, in
//! that order, whenever something needs the port and it isn't ready — and
//! keeping its checkout on upstream while the section is in use.

use std::time::Duration;

use decanter_ps::anyps5::AnyPs5;
use decanter_ps::bbport::Bbport;
use decanter_ps::port::Port;
use decanter_ps::toolchain::Toolchain;
use decanter_ps::updater::{self, Check, Plan};
use futures::FutureExt as _;
use futures::future::Shared;
use gpui::{AsyncApp, Context, Task, WeakEntity};

use super::Ps;
use super::jobs::{self, JobKey};

/// How often upstream is checked once the PS section has been opened.
const UPDATE_INTERVAL: Duration = Duration::from_secs(30 * 60);

pub type Prepared = Result<Toolchain, String>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Stage {
    /// Nothing has needed the port yet this session.
    Idle,
    Resolving,
    /// Fetching nix-portable: bytes so far, and the total when known.
    Downloading(u64, Option<u64>),
    Cloning,
    Building,
    Ready,
    Failed(String),
}

pub struct PortState {
    stage: Stage,
    toolchain: Option<Toolchain>,
    check: Option<Result<Check, String>>,
    preparing: Option<Shared<Task<Prepared>>>,
}

impl PortState {
    pub const fn new() -> Self {
        Self {
            stage: Stage::Idle,
            toolchain: None,
            check: None,
            preparing: None,
        }
    }

    pub const fn stage(&self) -> &Stage {
        &self.stage
    }

    pub const fn toolchain(&self) -> Option<&Toolchain> {
        self.toolchain.as_ref()
    }

    pub const fn check(&self) -> Option<&Result<Check, String>> {
        self.check.as_ref()
    }

    pub const fn is_busy(&self) -> bool {
        matches!(
            self.stage,
            Stage::Resolving | Stage::Downloading(..) | Stage::Cloning | Stage::Building
        )
    }
}

impl Ps {
    /// The port ready to use, preparing it first if needed. Callers that
    /// arrive while it is being prepared wait on the same preparation.
    pub fn prepare(&mut self, port: Port, cx: &mut Context<Self>) -> Shared<Task<Prepared>> {
        let state = self.ports.entry(port).or_insert_with(PortState::new);
        if let Some(preparing) = &state.preparing {
            return preparing.clone();
        }
        let task = cx.spawn(async move |this, cx| prepare(&this, port, cx).await).shared();
        state.preparing = Some(task.clone());
        // Dropped once done, so the next call checks the port again (a
        // build may have been removed, or a toolchain appeared).
        let done = task.clone();
        cx.spawn(async move |this, cx| {
            let _ = done.await;
            let _ = this.update(cx, |this, cx| {
                if let Some(state) = this.ports.get_mut(&port) {
                    state.preparing = None;
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
        task
    }

    /// The periodic upstream check, from the first time the section opens
    /// until decanter exits.
    pub(super) fn start_update_loop(cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            loop {
                for port in Port::ALL {
                    if this.upgrade().is_none() {
                        return;
                    }
                    check_and_update(&this, port, cx).await;
                }
                cx.background_executor().timer(UPDATE_INTERVAL).await;
            }
        })
        .detach();
    }

    /// The reset the Ports view offers when upstream rewrote history; it
    /// saves the local diff first.
    pub fn reset_to_upstream(&mut self, port: Port, cx: &mut Context<Self>) {
        let Some(Ok(check)) = self.ports.get(&port).and_then(|state| state.check.clone()) else {
            return;
        };
        cx.spawn(async move |this, cx| {
            let backup = updater::backup_path(port, &check.head);
            let command = updater::reset_command(&port.checkout(), &check.branch, &backup);
            let finished = jobs::run(&this, JobKey::Port(port), "restablecer", command, None, cx).await;
            if finished.succeeded() {
                rebuild(&this, port, cx).await;
            }
            check_and_update(&this, port, cx).await;
        })
        .detach();
    }
}

fn set_stage(this: &WeakEntity<Ps>, port: Port, stage: Stage, cx: &mut AsyncApp) {
    let _ = this.update(cx, |this, cx| {
        this.ports.entry(port).or_insert_with(PortState::new).stage = stage;
        cx.notify();
    });
}

fn fail(this: &WeakEntity<Ps>, port: Port, message: String, cx: &mut AsyncApp) -> Prepared {
    tracing::warn!(port = port.name(), %message, "port not ready");
    set_stage(this, port, Stage::Failed(message.clone()), cx);
    Err(message)
}

async fn prepare(this: &WeakEntity<Ps>, port: Port, cx: &mut AsyncApp) -> Prepared {
    set_stage(this, port, Stage::Resolving, cx);
    let progress = this.clone();
    let mut async_cx = cx.clone();
    let resolved = Toolchain::resolve(port, move |received, total| {
        set_stage(&progress, port, Stage::Downloading(received, total), &mut async_cx);
    })
    .await;
    let toolchain = match resolved {
        Ok(toolchain) => toolchain,
        Err(err) => return fail(this, port, err.to_string(), cx),
    };
    let _ = this.update(cx, |this, _| {
        this.ports.entry(port).or_insert_with(PortState::new).toolchain = Some(toolchain.clone());
    });

    let checkout = port.checkout();
    if !tokio::fs::try_exists(&checkout).await.unwrap_or(false) {
        set_stage(this, port, Stage::Cloning, cx);
        let finished = jobs::run(
            this,
            JobKey::Port(port),
            "clonar",
            updater::clone_command(port),
            None,
            cx,
        )
        .await;
        if !finished.succeeded() {
            return fail(this, port, format!("No se pudo clonar {}", port.name()), cx);
        }
    }

    let built = match port {
        Port::Bbport => Bbport::new(checkout).is_built_with(&toolchain).await,
        Port::AnyPs5 => AnyPs5::new(checkout).is_built_with(&toolchain).await,
    };
    if !built && let Err(message) = build(this, port, &toolchain, cx).await {
        return fail(this, port, message, cx);
    }
    set_stage(this, port, Stage::Ready, cx);
    Ok(toolchain)
}

async fn build(this: &WeakEntity<Ps>, port: Port, toolchain: &Toolchain, cx: &mut AsyncApp) -> Result<(), String> {
    set_stage(this, port, Stage::Building, cx);
    let checkout = port.checkout();
    let command = match port {
        Port::Bbport => Bbport::new(checkout.clone()).build_command(toolchain).await,
        Port::AnyPs5 => AnyPs5::new(checkout.clone()).build_command(toolchain).await,
    };
    let command = match command {
        Ok(command) => command,
        Err(err) => return Err(format!("No se pudo preparar el entorno: {err}")),
    };
    let finished = jobs::run(this, JobKey::Port(port), "compilar", command, None, cx).await;
    if !finished.succeeded() {
        return Err(format!("La compilación de {} falló; mira su registro", port.name()));
    }
    let recorded = match port {
        Port::Bbport => Bbport::new(checkout).record_build(toolchain).await,
        Port::AnyPs5 => AnyPs5::new(checkout).record_build(toolchain).await,
    };
    recorded.map_err(|err| format!("No se pudo registrar la compilación: {err}"))
}

/// After the checkout moved: built again right away, since whatever runs
/// next would otherwise build first anyway.
async fn rebuild(this: &WeakEntity<Ps>, port: Port, cx: &mut AsyncApp) {
    let toolchain = this
        .update(cx, |this, _| {
            this.ports.get(&port).and_then(|state| state.toolchain.clone())
        })
        .ok()
        .flatten();
    let Some(toolchain) = toolchain else {
        // Never prepared this session: the next prepare builds.
        return;
    };
    if let Err(message) = build(this, port, &toolchain, cx).await {
        set_stage(this, port, Stage::Failed(message), cx);
    } else {
        set_stage(this, port, Stage::Ready, cx);
    }
}

async fn check_and_update(this: &WeakEntity<Ps>, port: Port, cx: &mut AsyncApp) {
    let checkout = port.checkout();
    if !tokio::fs::try_exists(&checkout).await.unwrap_or(false) {
        return;
    }
    let check = updater::check(&checkout, port.upstream()).await;
    let fast_forward = matches!(&check, Ok(check) if check.plan == Plan::FastForward);
    let branch = check.as_ref().map(|check| check.branch.clone()).unwrap_or_default();
    // Never under a running game or build: they read the checkout.
    let idle = this
        .update(cx, |this, cx| {
            this.ports.entry(port).or_insert_with(PortState::new).check = Some(check);
            cx.notify();
            !this.jobs.values().any(|job| job.read(cx).is_running()) && !this.ports.values().any(PortState::is_busy)
        })
        .unwrap_or(false);
    if !(fast_forward && idle) {
        return;
    }
    let command = updater::fast_forward_command(&checkout, &branch);
    let finished = jobs::run(this, JobKey::Port(port), "actualizar", command, None, cx).await;
    if finished.succeeded() {
        tracing::info!(port = port.name(), "port fast-forwarded to upstream");
        rebuild(this, port, cx).await;
    }
    let check = updater::check(&checkout, port.upstream()).await;
    let _ = this.update(cx, |this, cx| {
        this.ports.entry(port).or_insert_with(PortState::new).check = Some(check);
        cx.notify();
    });
}
