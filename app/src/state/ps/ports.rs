//! Getting a port ready without asking: checkout, then Release's bundle
//! ([`super::bundle`]) or toolchain and build, whenever something needs the
//! port and it isn't ready — and keeping its checkout on upstream, or on its
//! latest release ([`super::channel`]), while the section is in use.

use std::io;
use std::time::Duration;

use decanter_ps::anyps5::AnyPs5;
use decanter_ps::bbport::{self, Bbport};
use decanter_ps::channel::Channel;
use decanter_ps::paths::{self, Root};
use decanter_ps::port::Port;
use decanter_ps::prebuilt;
use decanter_ps::process::JobKind;
use decanter_ps::remote::RemoteError;
use decanter_ps::toolchain::{self, Toolchain};
use decanter_ps::updater::{self, Check, Plan};
use futures::FutureExt as _;
use futures::future::Shared;
use gpui::{AppContext as _, AsyncApp, Context, Task, WeakEntity};
use rust_i18n::t;

use super::Ps;
use super::bundle;
use super::channel::{self, Track};
use super::fallback::{self, Fallback};
use super::jobs::{self, JobKey};
use crate::capabilities::Caps;

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
    /// Fetching the release's bundle ([`super::bundle`]): bytes so far, and the total when known.
    DownloadingBundle(u64, Option<u64>),
    Cloning,
    /// Fast-forwarding the checkout to upstream, and building it again if it was prepared.
    Updating,
    /// Fetching the checkout's commit as the port's CI built it.
    Fetching,
    Building,
    /// Putting the checkout back on the last commit that built, after the build failed.
    RollingBack,
    Ready,
    Failed(String),
}

/// Where the port's current build came from, for the Ports view.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuildOrigin {
    Local,
    Cache,
    /// The release's bundle, run without nix.
    Bundle,
}

pub struct PortState {
    stage: Stage,
    pub(super) origin: Option<BuildOrigin>,
    /// With the library it was resolved for, since nix-portable lives in each one.
    pub(super) toolchain: Option<(Root, Toolchain)>,
    pub(super) check: Option<Result<Check, RemoteError>>,
    pub(super) channel: Option<Channel>,
    pub(super) track: Option<Track>,
    /// An upstream check is running, from the periodic loop or the Ports view's button.
    checking: bool,
    /// When the last upstream check ended, so the Ports view tells a fresh answer from an old one.
    checked_at: Option<chrono::DateTime<chrono::Local>>,
    /// Release built locally because its prebuilt build was not there.
    pub(super) fell_back: bool,
    /// With the library it prepares in, since each library has its own checkout.
    preparing: Option<(Root, Shared<Task<Prepared>>)>,
    /// Whether the checkout moved.
    updating: Option<Shared<Task<bool>>>,
    pub(super) fallback: Option<Fallback>,
}

impl PortState {
    pub const fn new() -> Self {
        Self {
            stage: Stage::Idle,
            origin: None,
            toolchain: None,
            check: None,
            channel: None,
            track: None,
            checking: false,
            checked_at: None,
            fell_back: false,
            preparing: None,
            updating: None,
            fallback: None,
        }
    }

    pub const fn stage(&self) -> &Stage {
        &self.stage
    }

    pub const fn origin(&self) -> Option<BuildOrigin> {
        self.origin
    }

    pub fn toolchain(&self) -> Option<&Toolchain> {
        self.toolchain.as_ref().map(|(_, toolchain)| toolchain)
    }

    pub const fn check(&self) -> Option<&Result<Check, RemoteError>> {
        self.check.as_ref()
    }

    pub const fn is_checking(&self) -> bool {
        self.checking
    }

    pub const fn checked_at(&self) -> Option<chrono::DateTime<chrono::Local>> {
        self.checked_at
    }

    pub const fn channel(&self) -> Option<Channel> {
        self.channel
    }

    pub const fn track(&self) -> Option<&Track> {
        self.track.as_ref()
    }

    pub const fn fell_back(&self) -> bool {
        self.fell_back
    }

    pub const fn fallback(&self) -> Option<&Fallback> {
        self.fallback.as_ref()
    }

    pub const fn is_busy(&self) -> bool {
        matches!(
            self.stage,
            Stage::Resolving
                | Stage::Downloading(..)
                | Stage::DownloadingBundle(..)
                | Stage::Cloning
                | Stage::Updating
                | Stage::Fetching
                | Stage::Building
                | Stage::RollingBack
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
                    check_once(&this, port, &paths::active(), false, cx).await;
                }
                cx.background_executor().timer(UPDATE_INTERVAL).await;
            }
        })
        .detach();
    }

    /// The Ports view's button: asks upstream right away instead of waiting
    /// for the next periodic check, and moves the checkout as that check would.
    pub fn check_now(&mut self, port: Port, cx: &mut Context<Self>) {
        let root = paths::active();
        cx.spawn(async move |this, cx| check_once(&this, port, &root, true, cx).await)
            .detach();
    }

    /// After Ajustes saved another channel: the checkout moves to it, then
    /// the port is prepared on it.
    pub fn switch_channel(&mut self, port: Port, cx: &mut Context<Self>) {
        let root = paths::active();
        cx.spawn(async move |this, cx| {
            check_and_update(&this, port, &root, false, cx).await;
            let Ok(prepared) = this.update(cx, |this, cx| this.prepare(port, &root, cx)) else {
                return;
            };
            if let Err(err) = prepared.await {
                tracing::warn!(port = port.name(), error = err, "port not ready on its new channel");
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
            let facts = jobs::port_facts(&root, port, None).await;
            let finished = jobs::run(
                &this,
                &root,
                JobKey::Port(port),
                JobKind::Reset,
                facts,
                command,
                None,
                cx,
            )
            .await;
            if finished.succeeded() {
                rebuild(&this, port, &root, cx).await;
            }
            check_and_update(&this, port, &root, false, cx).await;
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

fn set_origin(this: &WeakEntity<Ps>, port: Port, origin: BuildOrigin, cx: &mut AsyncApp) {
    let _ = this.update(cx, |this, cx| {
        this.ports.entry(port).or_insert_with(PortState::new).origin = Some(origin);
        cx.notify();
    });
}

pub(super) fn set_stage(this: &WeakEntity<Ps>, port: Port, stage: Stage, cx: &mut AsyncApp) {
    let _ = this.update(cx, |this, cx| {
        this.ports.entry(port).or_insert_with(PortState::new).stage = stage;
        cx.notify();
    });
}

/// Why a port is not ready: logged as it is, worded for the UI by [`PortError::localized`].
#[derive(Debug, thiserror::Error)]
pub(super) enum PortError {
    #[error("toolchain unavailable: {0}")]
    Toolchain(toolchain::Error),
    #[error("git clone failed")]
    Clone,
    #[error("CMake cache reset failed: {0}")]
    CmakeCache(io::Error),
    #[error("build environment setup failed: {0}")]
    Environment(io::Error),
    #[error("build failed")]
    Build,
    #[error("build failed; local changes kept it off the last commit that built, {0}")]
    BuildNotRolledBack(String),
    #[error("could not put the checkout back on the last commit that built, {0}")]
    Rollback(String),
    #[error("build marker write failed: {0}")]
    RecordBuild(io::Error),
}

impl PortError {
    fn localized(&self, port: Port) -> String {
        match self {
            Self::Toolchain(err) => toolchain_error(err),
            Self::Clone => t!("ps.ports.errors.clone", port = port.name()).into_owned(),
            Self::CmakeCache(err) => t!("ps.ports.errors.cmake_cache", error = err).into_owned(),
            Self::Environment(err) => t!("ps.errors.environment", error = err).into_owned(),
            Self::Build => t!("ps.ports.errors.build", port = port.name()).into_owned(),
            Self::BuildNotRolledBack(commit) => {
                t!("ps.ports.errors.build_local", port = port.name(), commit = commit).into_owned()
            }
            Self::Rollback(commit) => t!("ps.ports.errors.rollback", port = port.name(), commit = commit).into_owned(),
            Self::RecordBuild(err) => t!("ps.ports.errors.record_build", error = err).into_owned(),
        }
    }
}

/// Returns what the port's stage now shows.
fn fail(this: &WeakEntity<Ps>, port: Port, err: &PortError, cx: &mut AsyncApp) -> String {
    tracing::warn!(port = port.name(), error = %err, "port not ready");
    let message = err.localized(port);
    set_stage(this, port, Stage::Failed(message.clone()), cx);
    message
}

async fn prepare(this: &WeakEntity<Ps>, port: Port, root: &Root, cx: &mut AsyncApp) -> Prepared {
    set_stage(this, port, Stage::Resolving, cx);
    // The clone needs only git, and goes first so Release's bundle can spare the toolchain.
    let checkout = port.checkout(root);
    if !tokio::fs::try_exists(&checkout).await.unwrap_or(false) {
        set_stage(this, port, Stage::Cloning, cx);
        let finished = jobs::run(
            this,
            root,
            JobKey::Port(port),
            JobKind::Clone,
            jobs::port_facts(root, port, None).await,
            updater::clone_command(root, port),
            None,
            cx,
        )
        .await;
        if !finished.succeeded() {
            return Err(fail(this, port, &PortError::Clone, cx));
        }
        if channel::load(this, port, root, cx).await == Channel::Release {
            channel::settle_clone(this, port, root, cx).await;
        }
    }
    if let Some(bundle) = bundle::ready(this, port, root, cx).await {
        fallback::settle(this, port, root, true, cx).await;
        ready(this, port, root, &bundle, cx).await;
        return Ok(bundle);
    }

    let progress = this.clone();
    let mut async_cx = cx.clone();
    let resolved = Toolchain::resolve(root, port, move |received, total| {
        set_stage(&progress, port, Stage::Downloading(received, total), &mut async_cx);
    })
    .await;
    let toolchain = match resolved {
        Ok(toolchain) => toolchain,
        Err(err) => return Err(fail(this, port, &PortError::Toolchain(err), cx)),
    };
    let _ = this.update(cx, |this, _| {
        this.ports.entry(port).or_insert_with(PortState::new).toolchain = Some((root.clone(), toolchain.clone()));
    });

    let built = match port {
        // Testing runs only what it built itself.
        Port::Bbport => {
            let bbport = Bbport::new(root.clone());
            bbport.is_built_with(&toolchain).await
                && (channel::load(this, port, root, cx).await == Channel::Release || bbport.prebuilt().await.is_none())
        }
        Port::AnyPs5 => AnyPs5::new(root.clone()).is_built_with(&toolchain).await,
    };
    let toolchain = if built {
        let origin = match port {
            Port::Bbport if Bbport::new(root.clone()).prebuilt().await.is_some() => BuildOrigin::Cache,
            Port::Bbport | Port::AnyPs5 => BuildOrigin::Local,
        };
        set_origin(this, port, origin, cx);
        toolchain
    } else {
        match build_or_fall_back(this, port, root, toolchain, cx).await {
            Ok(toolchain) => toolchain,
            Err(err) => {
                let message = fail(this, port, &err, cx);
                fallback::settle(this, port, root, false, cx).await;
                return Err(message);
            }
        }
    };
    fallback::settle(this, port, root, true, cx).await;
    ready(this, port, root, &toolchain, cx).await;
    Ok(toolchain)
}

/// `port` can run on `toolchain` now: what psport reports about this host
/// takes the place of what an earlier build reported.
async fn ready(this: &WeakEntity<Ps>, port: Port, root: &Root, toolchain: &Toolchain, cx: &mut AsyncApp) {
    set_stage(this, port, Stage::Ready, cx);
    if port != Port::Bbport {
        return;
    }
    let report = bbport::host_features(&Bbport::new(root.clone()), toolchain).await;
    cx.update(|app| Caps::take_psport_report(app, report.as_ref()));
}

/// [`build`], then [`fallback::recover`] when it is the build itself that failed.
async fn build_or_fall_back(
    this: &WeakEntity<Ps>,
    port: Port,
    root: &Root,
    toolchain: Toolchain,
    cx: &mut AsyncApp,
) -> Result<Toolchain, PortError> {
    match build(this, port, root, toolchain, cx).await {
        Err(PortError::Build) => fallback::recover(this, port, root, cx).await,
        built => built,
    }
}

/// Release's prebuilt build when it can be fetched, else why the port is
/// built locally, for the build log.
async fn prebuilt_or_reason(
    this: &WeakEntity<Ps>,
    port: Port,
    root: &Root,
    toolchain: &Toolchain,
    cx: &mut AsyncApp,
) -> Result<Toolchain, Option<&'static str>> {
    let channel = channel::load(this, port, root, cx).await;
    let fetched = match channel {
        Channel::Release => fetch_prebuilt(this, port, root, toolchain, cx).await,
        Channel::Testing => None,
    };
    let _ = this.update(cx, |this, cx| {
        let state = this.ports.entry(port).or_insert_with(PortState::new);
        state.fell_back = channel == Channel::Release && fetched.is_none();
        if let Some(nix) = &fetched {
            state.toolchain = Some((root.clone(), nix.clone()));
            state.origin = Some(BuildOrigin::Cache);
        }
        cx.notify();
    });
    match (channel, fetched) {
        (_, Some(nix)) => Ok(nix),
        (Channel::Release, None) => {
            tracing::warn!(
                port = port.name(),
                "release channel without a prebuilt build to fetch; building locally"
            );
            Err(Some("local: no prebuilt release build to fetch"))
        }
        (Channel::Testing, None) => Err(port.binary_cache().map(|_| "local: testing channel")),
    }
}

/// Returns the toolchain that built it, nix's when the host's tools could
/// not configure the port or when its build came from the binary cache.
pub(super) async fn build(
    this: &WeakEntity<Ps>,
    port: Port,
    root: &Root,
    mut toolchain: Toolchain,
    cx: &mut AsyncApp,
) -> Result<Toolchain, PortError> {
    let local_reason = match prebuilt_or_reason(this, port, root, &toolchain, cx).await {
        Ok(nix) => return Ok(nix),
        Err(reason) => reason,
    };
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
        Err(err) => return Err(PortError::CmakeCache(err)),
    }
    let mut fell_back = false;
    loop {
        let command = match port {
            Port::Bbport => Bbport::new(root.clone()).build_command(&toolchain).await,
            Port::AnyPs5 => AnyPs5::new(root.clone()).build_command(&toolchain).await,
        };
        let command = match command {
            Ok(command) => command,
            Err(err) => return Err(PortError::Environment(err)),
        };
        let mut facts = jobs::port_facts(root, port, Some(&toolchain)).await;
        facts.extend(local_reason.map(|reason| ("build", reason.to_owned())));
        let finished = jobs::run(this, root, JobKey::Port(port), JobKind::Build, facts, command, None, cx).await;
        if finished.succeeded() {
            break;
        }
        if toolchain != Toolchain::Host || !failed_to_configure(this, port, cx) {
            return Err(PortError::Build);
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
            return Err(PortError::CmakeCache(err));
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
            Err(err) => return Err(PortError::Toolchain(err)),
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
        Ok(()) => {
            set_origin(this, port, BuildOrigin::Local, cx);
            Ok(toolchain)
        }
        Err(err) => Err(PortError::RecordBuild(err)),
    }
}

/// The checkout's commit from the port's binary cache instead of a build,
/// when its CI published one. Run with the nix it was fetched into, whatever
/// toolchain was resolved: its binaries only load nix's libraries. `None`
/// for anything that stops it (no cache for the port, local changes, a
/// commit upstream lacks or CI didn't build, a failed fetch), and the port
/// is built as it always was.
async fn fetch_prebuilt(
    this: &WeakEntity<Ps>,
    port: Port,
    root: &Root,
    toolchain: &Toolchain,
    cx: &mut AsyncApp,
) -> Option<Toolchain> {
    let Some(cache) = prebuilt::cache(port) else {
        return None;
    };
    let bbport = Bbport::new(root.clone());
    let resolved_nix = match toolchain {
        Toolchain::Host | Toolchain::Bundle => None,
        Toolchain::SystemNix(_) | Toolchain::Portable(_) => Some(toolchain.clone()),
    };
    // The host is resolved again on every prepare, so a build fetched before is
    // found here, ahead of the network, and still launches offline.
    if resolved_nix.is_none()
        && bbport.prebuilt().await.is_some()
        && let Ok(nix) = Toolchain::nix(root, port, |_, _| {}).await
        && bbport.is_built_with(&nix).await
    {
        return Some(nix);
    }
    let Some(commit) = updater::upstream_commit(&port.checkout(root)).await else {
        return None;
    };
    let store_path = match prebuilt::lookup(&cache, &commit).await {
        Ok(Some(store_path)) => store_path,
        Ok(None) => {
            tracing::info!(
                port = port.name(),
                commit,
                "no prebuilt port for this commit; building it"
            );
            return None;
        }
        Err(err) => {
            tracing::warn!(port = port.name(), commit, %err, "binary cache lookup failed; building the port");
            return None;
        }
    };
    set_stage(this, port, Stage::Fetching, cx);
    let nix = match resolved_nix {
        Some(nix) => nix,
        None => {
            let progress = this.clone();
            let mut async_cx = cx.clone();
            let resolved = Toolchain::nix(root, port, move |received, total| {
                set_stage(&progress, port, Stage::Downloading(received, total), &mut async_cx);
            })
            .await;
            set_stage(this, port, Stage::Fetching, cx);
            match resolved {
                Ok(nix) => nix,
                Err(err) => {
                    tracing::warn!(port = port.name(), %err, "no nix to fetch the prebuilt port into; building it");
                    return None;
                }
            }
        }
    };
    let Some(command) = nix.fetch_command(root, &cache, &store_path, &prebuilt::gc_root(root, port)) else {
        return None;
    };
    let mut facts = jobs::port_facts(root, port, Some(&nix)).await;
    facts.extend([
        ("build", "prebuilt from cache".to_owned()),
        ("cache", cache),
        ("store_path", store_path.clone()),
    ]);
    let finished = jobs::run(this, root, JobKey::Port(port), JobKind::Build, facts, command, None, cx).await;
    if !finished.succeeded() {
        tracing::warn!(
            port = port.name(),
            store_path,
            "prebuilt port fetch failed; building it"
        );
        return None;
    }
    if let Err(err) = bbport.record_prebuilt(&nix, &store_path).await {
        tracing::warn!(port = port.name(), %err, "could not record the prebuilt port; building it");
        return None;
    }
    tracing::info!(
        port = port.name(),
        commit,
        store_path,
        toolchain = nix.id(),
        "port prebuilt from cache"
    );
    Some(nix)
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
    match Toolchain::allow_host(root, port).await {
        // Otherwise the host's configure would keep nix's compiler from the cache.
        Ok(true) => {
            let checkout = port.checkout(root);
            if let Err(err) = cx
                .background_spawn(async move { decanter_ps::cmake_cache::forget_all(&checkout) })
                .await
            {
                tracing::warn!(port = port.name(), %err, "could not drop nix's CMake cache for the host");
            }
        }
        Ok(false) => {}
        Err(err) => tracing::warn!(port = port.name(), %err, "could not let the host try the port again"),
    }
    let prepared_here = this
        .update(cx, |this, _| {
            this.ports
                .get(&port)
                .and_then(|state| state.toolchain.as_ref())
                .is_some_and(|(resolved_in, _)| resolved_in == root)
        })
        .unwrap_or(false);
    if !prepared_here {
        // Never prepared in this library this session: the next prepare builds.
        return;
    }
    if let Some(bundle) = bundle::ready(this, port, root, cx).await {
        fallback::settle(this, port, root, true, cx).await;
        ready(this, port, root, &bundle, cx).await;
        return;
    }
    // Resolved again rather than reusing nix from a fallback, now that the host may build it.
    let toolchain = match Toolchain::resolve(root, port, |_, _| {}).await {
        Ok(toolchain) => toolchain,
        Err(err) => {
            fail(this, port, &PortError::Toolchain(err), cx);
            return;
        }
    };
    let _ = this.update(cx, |this, _| {
        this.ports.entry(port).or_insert_with(PortState::new).toolchain = Some((root.clone(), toolchain.clone()));
    });
    match build_or_fall_back(this, port, root, toolchain, cx).await {
        Err(err) => {
            fail(this, port, &err, cx);
            fallback::settle(this, port, root, false, cx).await;
        }
        Ok(toolchain) => {
            fallback::settle(this, port, root, true, cx).await;
            ready(this, port, root, &toolchain, cx).await;
        }
    }
}

/// [`check_and_update`], skipped while another check of `port` runs so the
/// button and the periodic loop never fetch into the same checkout at once.
pub(super) async fn check_once(this: &WeakEntity<Ps>, port: Port, root: &Root, retry_failed: bool, cx: &mut AsyncApp) {
    let started = this
        .update(cx, |this, cx| {
            let state = this.ports.entry(port).or_insert_with(PortState::new);
            if state.checking {
                return false;
            }
            state.checking = true;
            cx.notify();
            true
        })
        .unwrap_or(false);
    if !started {
        return;
    }
    check_and_update(this, port, root, retry_failed, cx).await;
    let _ = this.update(cx, |this, cx| {
        let state = this.ports.entry(port).or_insert_with(PortState::new);
        state.checking = false;
        state.checked_at = Some(chrono::Local::now());
        cx.notify();
    });
}

/// `retry_failed` moves the checkout onto a commit that failed to build before too; the
/// automatic update leaves it alone.
pub(super) async fn check_and_update(
    this: &WeakEntity<Ps>,
    port: Port,
    root: &Root,
    retry_failed: bool,
    cx: &mut AsyncApp,
) {
    let checkout = port.checkout(root);
    if !tokio::fs::try_exists(&checkout).await.unwrap_or(false) {
        return;
    }
    if channel::load(this, port, root, cx).await == Channel::Release
        && channel::follow(this, port, root, retry_failed, cx).await
    {
        return;
    }
    let check = updater::check(&checkout, port.upstream()).await;
    let skipped = match &check {
        Ok(check) if !retry_failed && matches!(check.plan, Plan::FastForward | Plan::Rewritten) => {
            fallback::load(root, port, cx).await.failed_at(&check.upstream)
        }
        _ => false,
    };
    if skipped && let Ok(check) = &check {
        tracing::info!(
            port = port.name(),
            upstream = check.upstream,
            branch = check.branch,
            "upstream still at a commit that failed to build; not updating until it moves or a retry"
        );
    }
    let command = match &check {
        _ if skipped => None,
        Ok(check) if check.plan == Plan::FastForward => Some(updater::fast_forward_command(&checkout, &check.branch)),
        Ok(check) if check.plan == Plan::Rewritten && check.dirty.is_empty() => {
            let backup = updater::backup_path(root, port, &check.head);
            Some(updater::reset_command(&checkout, &check.branch, &backup))
        }
        _ => None,
    };
    let _ = this.update(cx, |this, cx| {
        this.ports.entry(port).or_insert_with(PortState::new).check = Some(check);
        cx.notify();
    });
    let Some(command) = command else {
        return;
    };
    let Some(updating) = start_update(this, port, root, command, cx) else {
        return;
    };
    updating.await;
    let check = updater::check(&checkout, port.upstream()).await;
    let _ = this.update(cx, |this, cx| {
        this.ports.entry(port).or_insert_with(PortState::new).check = Some(check);
        cx.notify();
    });
}

/// Moves the checkout with `command` and builds it again, unless a game or a
/// build is running: they read the checkout. Held as the port's `updating`
/// task, so a prepare arriving meanwhile waits for it instead of building
/// the checkout git is moving.
pub(super) fn start_update(
    this: &WeakEntity<Ps>,
    port: Port,
    root: &Root,
    command: tokio::process::Command,
    cx: &mut AsyncApp,
) -> Option<Shared<Task<bool>>> {
    this.update(cx, |this, cx| {
        let idle =
            !this.jobs.values().any(|job| job.read(cx).is_running()) && !this.ports.values().any(PortState::is_busy);
        if !idle {
            return None;
        }
        let state = this.ports.entry(port).or_insert_with(PortState::new);
        let previous = std::mem::replace(&mut state.stage, Stage::Updating);
        let root = root.clone();
        let task = cx
            .spawn(async move |this, cx| apply_update(&this, port, &root, command, previous, cx).await)
            .shared();
        state.updating = Some(task.clone());
        cx.notify();
        Some(task)
    })
    .ok()
    .flatten()
}

async fn apply_update(
    this: &WeakEntity<Ps>,
    port: Port,
    root: &Root,
    command: tokio::process::Command,
    previous: Stage,
    cx: &mut AsyncApp,
) -> bool {
    let facts = jobs::port_facts(root, port, None).await;
    let finished = jobs::run(
        this,
        root,
        JobKey::Port(port),
        JobKind::Update,
        facts,
        command,
        None,
        cx,
    )
    .await;
    let moved = finished.succeeded();
    if moved {
        tracing::info!(port = port.name(), "port checkout moved");
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
    moved
}
