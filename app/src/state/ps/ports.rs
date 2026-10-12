//! Getting a port ready without asking: toolchain, checkout and build, in
//! that order, whenever something needs the port and it isn't ready — and
//! keeping its checkout on upstream while the section is in use.

use std::time::Duration;

use decanter_ps::anyps5::AnyPs5;
use decanter_ps::bbport::Bbport;
use decanter_ps::paths::{self, Root};
use decanter_ps::port::Port;
use decanter_ps::toolchain::{self, Toolchain};
use decanter_ps::updater::{self, Check, Plan};
use futures::FutureExt as _;
use futures::future::Shared;
use gpui::{AppContext as _, AsyncApp, Context, Task, WeakEntity};
use rust_i18n::t;

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
    /// Fast-forwarding the checkout to upstream, and building it again if it was prepared.
    Updating,
    Building,
    Ready,
    Failed(String),
}

pub struct PortState {
    stage: Stage,
    /// With the library it was resolved for, since nix-portable lives in each one.
    toolchain: Option<(Root, Toolchain)>,
    check: Option<Result<Check, String>>,
    /// With the library it prepares in, since each library has its own checkout.
    preparing: Option<(Root, Shared<Task<Prepared>>)>,
    updating: Option<Shared<Task<()>>>,
}

impl PortState {
    pub const fn new() -> Self {
        Self {
            stage: Stage::Idle,
            toolchain: None,
            check: None,
            preparing: None,
            updating: None,
        }
    }

    pub const fn stage(&self) -> &Stage {
        &self.stage
    }

    pub fn toolchain(&self) -> Option<&Toolchain> {
        self.toolchain.as_ref().map(|(_, toolchain)| toolchain)
    }

    pub const fn check(&self) -> Option<&Result<Check, String>> {
        self.check.as_ref()
    }

    pub const fn is_busy(&self) -> bool {
        matches!(
            self.stage,
            Stage::Resolving | Stage::Downloading(..) | Stage::Cloning | Stage::Updating | Stage::Building
        )
    }
}

impl Ps {
    /// The port ready to use in `root`, preparing it first if needed.
    /// Callers that arrive while it is being prepared there wait on the same
    /// preparation; one for another library waits for it to end first.
    pub fn prepare(&mut self, port: Port, root: &Root, cx: &mut Context<Self>) -> Shared<Task<Prepared>> {
        let state = self.ports.entry(port).or_insert_with(PortState::new);
        let before = match &state.preparing {
            Some((preparing_root, preparing)) if preparing_root == root => return preparing.clone(),
            Some((_, preparing)) => Some(preparing.clone()),
            None => None,
        };
        let updating = state.updating.clone();
        let task = cx
            .spawn({
                let root = root.clone();
                async move |this, cx| {
                    if let Some(updating) = updating {
                        updating.await;
                    }
                    if let Some(before) = before {
                        let _ = before.await;
                    }
                    prepare(&this, port, &root, cx).await
                }
            })
            .shared();
        state.preparing = Some((root.clone(), task.clone()));
        // Dropped once done, so the next call checks the port again (a
        // build may have been removed, or a toolchain appeared).
        let done = task.clone();
        cx.spawn(async move |this, cx| {
            let _ = done.clone().await;
            let _ = this.update(cx, |this, cx| {
                if let Some(state) = this.ports.get_mut(&port)
                    && state
                        .preparing
                        .as_ref()
                        .is_some_and(|(_, preparing)| preparing.ptr_eq(&done))
                {
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
                    check_and_update(&this, port, &paths::active(), cx).await;
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
        let root = paths::active();
        cx.spawn(async move |this, cx| {
            let backup = updater::backup_path(&root, port, &check.head);
            let command = updater::reset_command(&port.checkout(&root), &check.branch, &backup);
            let finished = jobs::run(
                &this,
                &root,
                JobKey::Port(port),
                &t!("ps.jobs.reset"),
                command,
                None,
                cx,
            )
            .await;
            if finished.succeeded() {
                rebuild(&this, port, &root, cx).await;
            }
            check_and_update(&this, port, &root, cx).await;
        })
        .detach();
    }
}

fn toolchain_error(err: &toolchain::Error) -> String {
    match err {
        toolchain::Error::Download(err) => t!("ps.ports.errors.download", error = err).into_owned(),
        toolchain::Error::Checksum => t!("ps.ports.errors.checksum").into_owned(),
        toolchain::Error::Io(err) => err.to_string(),
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

async fn prepare(this: &WeakEntity<Ps>, port: Port, root: &Root, cx: &mut AsyncApp) -> Prepared {
    set_stage(this, port, Stage::Resolving, cx);
    let progress = this.clone();
    let mut async_cx = cx.clone();
    let resolved = Toolchain::resolve(root, port, move |received, total| {
        set_stage(&progress, port, Stage::Downloading(received, total), &mut async_cx);
    })
    .await;
    let toolchain = match resolved {
        Ok(toolchain) => toolchain,
        Err(err) => return fail(this, port, toolchain_error(&err), cx),
    };
    let _ = this.update(cx, |this, _| {
        this.ports.entry(port).or_insert_with(PortState::new).toolchain = Some((root.clone(), toolchain.clone()));
    });

    let checkout = port.checkout(root);
    if !tokio::fs::try_exists(&checkout).await.unwrap_or(false) {
        set_stage(this, port, Stage::Cloning, cx);
        let finished = jobs::run(
            this,
            root,
            JobKey::Port(port),
            &t!("ps.jobs.clone"),
            updater::clone_command(root, port),
            None,
            cx,
        )
        .await;
        if !finished.succeeded() {
            return fail(
                this,
                port,
                t!("ps.ports.errors.clone", port = port.name()).into_owned(),
                cx,
            );
        }
    }

    let built = match port {
        Port::Bbport => Bbport::new(root.clone()).is_built_with(&toolchain).await,
        Port::AnyPs5 => AnyPs5::new(root.clone()).is_built_with(&toolchain).await,
    };
    let toolchain = if built {
        toolchain
    } else {
        match build(this, port, root, toolchain, cx).await {
            Ok(toolchain) => toolchain,
            Err(message) => return fail(this, port, message, cx),
        }
    };
    set_stage(this, port, Stage::Ready, cx);
    Ok(toolchain)
}

/// Returns the toolchain that built it, nix's when the host's tools could
/// not configure the port.
async fn build(
    this: &WeakEntity<Ps>,
    port: Port,
    root: &Root,
    mut toolchain: Toolchain,
    cx: &mut AsyncApp,
) -> Result<Toolchain, String> {
    set_stage(this, port, Stage::Building, cx);
    let for_reset = port.checkout(root);
    match cx
        .background_spawn(async move { decanter_ps::cmake_cache::forget_moved(&for_reset) })
        .await
    {
        Ok(reset) => {
            for dir in reset {
                tracing::info!(port = port.name(), dir = %dir.display(), "CMake cache from before a move reset");
            }
        }
        Err(err) => return Err(t!("ps.ports.errors.cmake_cache", error = err).into_owned()),
    }
    let mut fell_back = false;
    loop {
        let command = match port {
            Port::Bbport => Bbport::new(root.clone()).build_command(&toolchain).await,
            Port::AnyPs5 => AnyPs5::new(root.clone()).build_command(&toolchain).await,
        };
        let command = match command {
            Ok(command) => command,
            Err(err) => return Err(t!("ps.errors.environment", error = err).into_owned()),
        };
        let finished = jobs::run(this, root, JobKey::Port(port), &t!("ps.jobs.build"), command, None, cx).await;
        if finished.succeeded() {
            break;
        }
        if toolchain != Toolchain::Host || !failed_to_configure(this, port, cx) {
            return Err(t!("ps.ports.errors.build", port = port.name()).into_owned());
        }
        tracing::warn!(
            port = port.name(),
            "CMake could not configure the port with the host's tools (a missing dependency); building with nix"
        );
        let checkout = port.checkout(root);
        if let Err(err) = cx
            .background_spawn(async move { decanter_ps::cmake_cache::forget_all(&checkout) })
            .await
        {
            return Err(t!("ps.ports.errors.cmake_cache", error = err).into_owned());
        }
        fell_back = true;
        let progress = this.clone();
        let mut async_cx = cx.clone();
        let resolved = Toolchain::nix(root, port, move |received, total| {
            set_stage(&progress, port, Stage::Downloading(received, total), &mut async_cx);
        })
        .await;
        toolchain = match resolved {
            Ok(nix) => nix,
            Err(err) => return Err(toolchain_error(&err)),
        };
        set_stage(this, port, Stage::Building, cx);
        let _ = this.update(cx, |this, _| {
            this.ports.entry(port).or_insert_with(PortState::new).toolchain = Some((root.clone(), toolchain.clone()));
        });
    }
    // Only once nix built it, so a CMake error nix shares doesn't keep the host away.
    if fell_back && let Err(err) = Toolchain::refuse_host(root, port).await {
        tracing::warn!(port = port.name(), %err, "could not record that the host can't build the port");
    }
    let recorded = match port {
        Port::Bbport => Bbport::new(root.clone()).record_build(&toolchain).await,
        Port::AnyPs5 => AnyPs5::new(root.clone()).record_build(&toolchain).await,
    };
    match recorded {
        Ok(()) => Ok(toolchain),
        Err(err) => Err(t!("ps.ports.errors.record_build", error = err).into_owned()),
    }
}

/// `CMake Error` marks a configure step that stopped, as opposed to a
/// compile error, which nix would hit just the same.
fn failed_to_configure(this: &WeakEntity<Ps>, port: Port, cx: &mut AsyncApp) -> bool {
    this.update(cx, |this, cx| {
        this.jobs
            .get(&JobKey::Port(port))
            .is_some_and(|job| job.read(cx).lines.iter().any(|line| line.contains("CMake Error")))
    })
    .unwrap_or(false)
}

/// After the checkout moved: built again right away, since whatever runs
/// next would otherwise build first anyway.
async fn rebuild(this: &WeakEntity<Ps>, port: Port, root: &Root, cx: &mut AsyncApp) {
    let toolchain = this
        .update(cx, |this, _| {
            this.ports.get(&port).and_then(|state| state.toolchain.clone())
        })
        .ok()
        .flatten();
    let Some((_, toolchain)) = toolchain.filter(|(resolved_in, _)| resolved_in == root) else {
        // Never prepared in this library this session: the next prepare builds.
        return;
    };
    if let Err(message) = build(this, port, root, toolchain, cx).await {
        set_stage(this, port, Stage::Failed(message), cx);
    } else {
        set_stage(this, port, Stage::Ready, cx);
    }
}

async fn check_and_update(this: &WeakEntity<Ps>, port: Port, root: &Root, cx: &mut AsyncApp) {
    let checkout = port.checkout(root);
    if !tokio::fs::try_exists(&checkout).await.unwrap_or(false) {
        return;
    }
    let check = updater::check(&checkout, port.upstream()).await;
    let fast_forward = matches!(&check, Ok(check) if check.plan == Plan::FastForward);
    let branch = check.as_ref().map(|check| check.branch.clone()).unwrap_or_default();
    let updating = this
        .update(cx, |this, cx| {
            // Never under a running game or build: they read the checkout.
            let idle = !this.jobs.values().any(|job| job.read(cx).is_running())
                && !this.ports.values().any(PortState::is_busy);
            let state = this.ports.entry(port).or_insert_with(PortState::new);
            state.check = Some(check);
            cx.notify();
            if !(fast_forward && idle) {
                return None;
            }
            let previous = std::mem::replace(&mut state.stage, Stage::Updating);
            let root = root.clone();
            let task = cx
                .spawn(async move |this, cx| apply_fast_forward(&this, port, &root, &branch, previous, cx).await)
                .shared();
            state.updating = Some(task.clone());
            Some(task)
        })
        .ok()
        .flatten();
    let Some(updating) = updating else {
        return;
    };
    updating.await;
    let check = updater::check(&checkout, port.upstream()).await;
    let _ = this.update(cx, |this, cx| {
        this.ports.entry(port).or_insert_with(PortState::new).check = Some(check);
        cx.notify();
    });
}

/// Held as the port's `updating` task, so a prepare arriving meanwhile
/// waits for it instead of building the checkout git is moving.
async fn apply_fast_forward(
    this: &WeakEntity<Ps>,
    port: Port,
    root: &Root,
    branch: &str,
    previous: Stage,
    cx: &mut AsyncApp,
) {
    let command = updater::fast_forward_command(&port.checkout(root), branch);
    let finished = jobs::run(this, root, JobKey::Port(port), &t!("ps.jobs.update"), command, None, cx).await;
    if finished.succeeded() {
        tracing::info!(port = port.name(), "port fast-forwarded to upstream");
        rebuild(this, port, root, cx).await;
    }
    let _ = this.update(cx, |this, cx| {
        let state = this.ports.entry(port).or_insert_with(PortState::new);
        if state.stage == Stage::Updating {
            state.stage = previous;
        }
        state.updating = None;
        cx.notify();
    });
}
