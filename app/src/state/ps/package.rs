//! psport's Windows package ([`decanter_ps::os::windows::package`]) where decanter runs `psport.exe` directly: the
//! releases that carry it, the versions in the library, which one Jugar runs, and keeping that one
//! on the newest release unless the user chose another.

use std::io;
use std::time::Duration;

use decanter_core::runner::{Pin, Runner, Tag};
use decanter_ps::bbport::LauncherSettings;
use decanter_ps::bbport::direct::FOLDER_KEY;
use decanter_ps::download;
use decanter_ps::os::windows::package::{self, Installed, Offer, PINNED_KEY, SELECTED_KEY, Version};
use decanter_ps::os::windows::winbuild::Commit;
use decanter_ps::paths::{self, Root};
use decanter_ps::port::Port;
use futures::FutureExt as _;
use futures::future::Shared;
use gpui::{AppContext as _, AsyncApp, Context, Task, WeakEntity};

use super::Ps;
use crate::i18n::Text;
use crate::runners;

/// As often as the ports' upstream check on Linux.
const UPDATE_INTERVAL: Duration = Duration::from_secs(30 * 60);

type Installing = Shared<Task<Result<Installed, Text>>>;

#[derive(Default)]
pub struct PackageState {
    /// The releases that carry the package, newest first, from the last check.
    offers: Option<Result<Vec<Offer>, Text>>,
    installed: Vec<Installed>,
    selected: Option<Version>,
    /// A build of psport's branch ([`super::branch`]) chosen instead of a release.
    selected_build: Option<Commit>,
    pinned: bool,
    /// The `psport.exe` folder picked in Ajustes, which Jugar runs instead.
    manual_folder: Option<String>,
    checking: bool,
    checked_at: Option<chrono::DateTime<chrono::Local>>,
    /// The version downloading: bytes so far, and the total when known.
    downloading: Option<(Version, u64, Option<u64>)>,
    error: Option<Text>,
    install: Option<(Version, Installing)>,
    /// The newest release whose automatic install failed, which the periodic check leaves alone
    /// until a newer one appears or the user checks by hand.
    failed: Option<Version>,
    /// Selections being saved, which a delete waits out.
    selecting: u32,
}

impl PackageState {
    pub const fn offers(&self) -> Option<&Result<Vec<Offer>, Text>> {
        self.offers.as_ref()
    }

    pub fn installed(&self) -> &[Installed] {
        &self.installed
    }

    pub const fn selected(&self) -> Option<&Version> {
        self.selected.as_ref()
    }

    pub const fn selected_build(&self) -> Option<&Commit> {
        self.selected_build.as_ref()
    }

    /// A choice of version or build is being saved.
    pub(super) const fn is_selecting(&self) -> bool {
        self.selecting > 0
    }

    pub const fn is_pinned(&self) -> bool {
        self.pinned
    }

    pub fn manual_folder(&self) -> Option<&str> {
        self.manual_folder.as_deref()
    }

    pub const fn is_checking(&self) -> bool {
        self.checking
    }

    pub const fn checked_at(&self) -> Option<chrono::DateTime<chrono::Local>> {
        self.checked_at
    }

    pub const fn downloading(&self) -> Option<&(Version, u64, Option<u64>)> {
        self.downloading.as_ref()
    }

    pub const fn error(&self) -> Option<&Text> {
        self.error.as_ref()
    }

    /// The newest release known: listed, else in the library when the releases couldn't be asked.
    fn newest(&self) -> Option<&Version> {
        match &self.offers {
            Some(Ok(offers)) => offers.first().map(|offer| &offer.version),
            Some(Err(_)) | None => self.installed.first().map(|installed| &installed.version),
        }
    }
}

impl Ps {
    pub const fn package(&self) -> &PackageState {
        &self.package
    }

    /// The periodic release check, from the first time the section opens until decanter exits.
    pub(super) fn start_package_loop(cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            loop {
                if this.upgrade().is_none() {
                    return;
                }
                check(&this, &paths::active(), Asked::Periodically, cx).await;
                cx.background_executor().timer(UPDATE_INTERVAL).await;
            }
        })
        .detach();
    }

    /// The Ports view's button: asks for the releases now, and follows the newest as the periodic
    /// check would.
    pub fn package_check(&mut self, cx: &mut Context<Self>) {
        let root = paths::active();
        cx.spawn(async move |this, cx| check(&this, &root, Asked::ByUser, cx).await)
            .detach();
    }

    /// What is in the library and chosen, read again without asking the network.
    pub fn package_reload(&mut self, cx: &mut Context<Self>) {
        let root = paths::active();
        cx.spawn(async move |this, cx| reload(&this, &root, cx).await).detach();
    }

    pub fn package_download(&mut self, version: &Version, cx: &mut Context<Self>) {
        let Some(offer) = self
            .package
            .offers
            .as_ref()
            .and_then(|offers| offers.as_ref().ok())
            .and_then(|offers| offers.iter().find(|offer| offer.version == *version))
            .cloned()
        else {
            return;
        };
        let root = paths::active();
        cx.spawn(async move |this, cx| {
            let task = match start_install(&this, &root, offer, cx) {
                Ok(task) => task,
                Err(err) => return set_error(&this, Some(err), cx),
            };
            let _ = task.await;
        })
        .detach();
    }

    /// Jugar runs `version` from now on. Chosen over the newest release, it stays until the user
    /// picks the newest again; the periodic check doesn't move it.
    pub fn package_use(&mut self, version: &Version, cx: &mut Context<Self>) {
        let pinned = self.package.newest() != Some(version);
        let (root, version) = (paths::active(), version.clone());
        cx.spawn(async move |this, cx| {
            if let Err(err) = select(&this, &root, version.tag(), pinned, cx).await {
                set_error(&this, Some(err), cx);
            }
        })
        .detach();
    }

    /// Never the version in use as the settings hold it, nor any while a selection is being saved or
    /// a game runs, since it may run from it.
    pub fn package_delete(&mut self, version: &Version, cx: &mut Context<Self>) {
        let playing = self.jobs.values().any(|job| job.read(cx).is_running());
        if self.package.selected.as_ref() == Some(version) || self.package.selecting > 0 || playing {
            self.package.error = Some(Text::new("ps.package.errors.delete_in_use"));
            cx.notify();
            return;
        }
        let (root, version) = (paths::active(), version.clone());
        cx.spawn(async move |this, cx| {
            if settings_now(&root, cx).await.selected.as_ref() == Some(&version) {
                set_error(&this, Some(Text::new("ps.package.errors.delete_in_use")), cx);
                return reload(&this, &root, cx).await;
            }
            // A version's tag is always a release's name; refused like a failed removal otherwise.
            let removed = match Tag::try_new(version.tag()) {
                Some(tag) => runners::remove(&root, Runner::Psport, &Pin::Release(tag)).await,
                None => Err(runners::Error::RemoveRelease(
                    Runner::Psport,
                    io::Error::from(io::ErrorKind::InvalidInput),
                )),
            };
            match removed {
                Ok(()) => {
                    tracing::info!(tag = version.tag(), "psport Windows package removed");
                    set_error(&this, None, cx);
                }
                Err(err) => {
                    tracing::warn!(tag = version.tag(), %err, "could not remove a psport Windows package");
                    set_error(&this, Some(err.text()), cx);
                }
            }
            reload(&this, &root, cx).await;
        })
        .detach();
    }
}

fn set_error(this: &WeakEntity<Ps>, error: Option<Text>, cx: &mut AsyncApp) {
    let _ = this.update(cx, |this, cx| {
        this.package.error = error;
        cx.notify();
    });
}

/// Why the package can't be listed or installed, in the UI's language.
fn package_error(err: &package::Error) -> Text {
    let offline = |request: &reqwest::Error| request.is_connect() || request.is_timeout();
    match err {
        package::Error::NotPublished => Text::new("ps.package.errors.not_published"),
        package::Error::Request(request) | package::Error::Download(download::Error::Download(request))
            if offline(request) =>
        {
            Text::new("ps.package.errors.offline")
        }
        package::Error::RateLimited => Text::new("ps.package.errors.rate_limited"),
        package::Error::NoChecksum(tag) => Text::new("ps.package.errors.no_checksum").arg("tag", tag),
        package::Error::Download(download::Error::Checksum | download::Error::Size { .. }) => {
            Text::new("ps.package.errors.checksum").arg("error", err)
        }
        package::Error::Layout(what) => Text::new("ps.package.errors.layout").arg("what", what),
        package::Error::Request(_)
        | package::Error::MalformedListing(_)
        | package::Error::Download(_)
        | package::Error::Tar(_)
        | package::Error::Io(_) => Text::new("ps.package.errors.failed").arg("error", err),
    }
}

/// Who asked for a check: a failed automatic install is retried only when the user asks.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Asked {
    Periodically,
    ByUser,
}

/// What the port settings say about the package now, read from disk rather than from the state,
/// which a save under way may not have reached yet.
pub(super) struct Choice {
    pub(super) selected: Option<Version>,
    pub(super) selected_build: Option<Commit>,
    pub(super) pinned: bool,
    pub(super) manual_folder: Option<String>,
}

pub(super) async fn settings_now(root: &Root, cx: &AsyncApp) -> Choice {
    let settings = {
        let root = root.clone();
        cx.background_spawn(async move { LauncherSettings::load(&root) }).await
    };
    Choice {
        selected: settings.text(SELECTED_KEY).and_then(|tag| tag.parse::<Version>().ok()),
        selected_build: settings.text(SELECTED_KEY).and_then(Commit::try_new),
        pinned: settings.flag(PINNED_KEY, false),
        manual_folder: settings.text(FOLDER_KEY).map(str::to_owned),
    }
}

pub(super) async fn reload(this: &WeakEntity<Ps>, root: &Root, cx: &mut AsyncApp) {
    let Choice {
        selected,
        selected_build,
        pinned,
        manual_folder,
    } = settings_now(root, cx).await;
    let installed = package::installed(root).await;
    let _ = this.update(cx, |this, cx| {
        let state = &mut this.package;
        state.installed = installed;
        state.selected = selected;
        state.selected_build = selected_build;
        state.pinned = pinned;
        state.manual_folder = manual_folder;
        cx.notify();
    });
}

/// The releases again, then the newest installed and chosen unless the user chose another version
/// or a folder of their own, or its install failed before and nobody asked again. Skipped while
/// another check runs.
async fn check(this: &WeakEntity<Ps>, root: &Root, asked: Asked, cx: &mut AsyncApp) {
    let started = this
        .update(cx, |this, cx| {
            if this.package.checking {
                return false;
            }
            this.package.checking = true;
            cx.notify();
            true
        })
        .unwrap_or(false);
    if !started {
        return;
    }
    reload(this, root, cx).await;
    let offers = package::offers(Port::Bbport).await;
    let newest = match &offers {
        Ok(offers) => offers.first().cloned(),
        Err(err) => {
            tracing::warn!(%err, "could not list psport's Windows packages");
            None
        }
    };
    let follow = this
        .update(cx, |this, cx| {
            let state = &mut this.package;
            state.offers = Some(offers.map_err(|err| package_error(&err)));
            state.checking = false;
            state.checked_at = Some(chrono::Local::now());
            cx.notify();
            if asked == Asked::ByUser {
                state.failed = None;
            }
            newest.filter(|newest| {
                !state.pinned
                    && state.manual_folder.is_none()
                    && state.selected.as_ref() != Some(&newest.version)
                    && state.failed.as_ref() != Some(&newest.version)
            })
        })
        .ok()
        .flatten();
    if let Some(newest) = follow {
        follow_newest(this, root, newest, cx).await;
    }
}

/// `newest` installed and chosen, unless the user chose a version or a folder while it downloaded.
async fn follow_newest(this: &WeakEntity<Ps>, root: &Root, newest: Offer, cx: &mut AsyncApp) {
    tracing::info!(tag = newest.version.tag(), "following psport's newest Windows release");
    let version = newest.version.clone();
    let task = match start_install(this, root, newest, cx) {
        Ok(task) => task,
        Err(err) => return set_error(this, Some(err), cx),
    };
    let installed = task.await;
    let _ = this.update(cx, |this, _| {
        this.package.failed = installed.is_err().then_some(version);
    });
    let Ok(installed) = installed else {
        return;
    };
    // Read again, since the user may have chosen a version or a folder while it downloaded.
    let choice = settings_now(root, cx).await;
    if choice.pinned || choice.manual_folder.is_some() {
        tracing::info!(
            tag = installed.version.tag(),
            "a version was chosen meanwhile; not switching to the new one"
        );
        return reload(this, root, cx).await;
    }
    if let Err(err) = select(this, root, installed.version.tag(), false, cx).await {
        set_error(this, Some(err), cx);
    }
}

/// Jugar runs `tag`, a release's tag or a build's commit, from now on.
pub(super) async fn select(
    this: &WeakEntity<Ps>,
    root: &Root,
    tag: &str,
    pinned: bool,
    cx: &mut AsyncApp,
) -> Result<(), Text> {
    let _ = this.update(cx, |this, _| {
        this.package.selecting = this.package.selecting.saturating_add(1);
    });
    let saved = {
        let (root, tag) = (root.clone(), tag.to_owned());
        cx.background_spawn(async move {
            LauncherSettings::edit(&root, |settings| {
                settings.set_text(SELECTED_KEY, &tag);
                if pinned {
                    settings.set_flag(PINNED_KEY, true);
                } else {
                    settings.remove(PINNED_KEY);
                }
            })
        })
        .await
    };
    let result = match saved {
        Ok(()) => {
            tracing::info!(tag, pinned, "psport Windows package in use");
            Ok(())
        }
        Err(err) => {
            tracing::warn!(tag, %err, "could not save which psport Windows package is in use");
            Err(Text::new("ps.settings.save_error").arg("error", err))
        }
    };
    reload(this, root, cx).await;
    let _ = this.update(cx, |this, _| {
        this.package.selecting = this.package.selecting.saturating_sub(1);
    });
    result
}

/// The install of `offer` under way, or a new one; one version at a time, so another one waits for
/// the user to try again.
fn start_install(this: &WeakEntity<Ps>, root: &Root, offer: Offer, cx: &mut AsyncApp) -> Result<Installing, Text> {
    let started = this.update(cx, |this, cx| {
        if let Some((version, task)) = &this.package.install {
            return if *version == offer.version {
                Ok(task.clone())
            } else {
                Err(Text::new("ps.package.errors.busy").arg("tag", version))
            };
        }
        let version = offer.version.clone();
        let root = root.clone();
        let task = cx
            .spawn(async move |this, cx| run_install(&this, &root, offer, cx).await)
            .shared();
        this.package.install = Some((version, task.clone()));
        cx.notify();
        Ok(task)
    });
    started.unwrap_or_else(|_| Err(Text::new("ps.package.errors.busy").arg("tag", "")))
}

async fn run_install(this: &WeakEntity<Ps>, root: &Root, offer: Offer, cx: &mut AsyncApp) -> Result<Installed, Text> {
    let version = offer.version.clone();
    let _ = this.update(cx, |this, cx| {
        this.package.downloading = Some((version.clone(), 0, None));
        this.package.error = None;
        cx.notify();
    });
    let progress = this.clone();
    let mut async_cx = cx.clone();
    let for_progress = version.clone();
    let installed = package::install(root, Port::Bbport, &offer, move |received, total| {
        let _ = progress.update(&mut async_cx, |this, cx| {
            this.package.downloading = Some((for_progress.clone(), received, total));
            cx.notify();
        });
    })
    .await;
    let result = installed.map_err(|err| {
        tracing::warn!(tag = version.tag(), %err, "psport Windows package not installed");
        package_error(&err)
    });
    let _ = this.update(cx, |this, cx| {
        let state = &mut this.package;
        state.downloading = None;
        state.install = None;
        state.error = result.as_ref().err().cloned();
        cx.notify();
    });
    reload(this, root, cx).await;
    result
}

/// The folder of the build or version Jugar runs, downloading the newest release first when neither
/// is in the library yet.
#[cfg(windows)]
pub(super) async fn for_play(
    this: &WeakEntity<Ps>,
    root: &Root,
    settings: &LauncherSettings,
    cx: &mut AsyncApp,
) -> Result<std::path::PathBuf, Text> {
    if let Some(commit) = settings.text(SELECTED_KEY).and_then(Commit::try_new)
        && decanter_ps::os::windows::winbuild::built_commit(root, &commit)
            .await
            .is_some()
    {
        return Ok(decanter_ps::os::windows::winbuild::dir(root, &commit));
    }
    let selected = settings.text(SELECTED_KEY).and_then(|tag| tag.parse::<Version>().ok());
    if let Some(version) = selected
        && package::installed_version(root, &version).await.is_some()
    {
        return Ok(package::dir(root, &version));
    }
    let offers = match package::offers(Port::Bbport).await {
        Ok(offers) => offers,
        Err(err) => {
            tracing::warn!(%err, "no psport Windows package to play with");
            return Err(Text::new("ps.play.package.unavailable").text("error", package_error(&err)));
        }
    };
    let Some(newest) = offers.first().cloned() else {
        tracing::info!("no psport release carries a Windows package yet");
        return Err(Text::new("ps.play.package.none"));
    };
    let _ = this.update(cx, |this, cx| {
        this.package.offers = Some(Ok(offers));
        this.notice = Some((
            Text::new("ps.play.package.downloading").arg("tag", &newest.version),
            false,
        ));
        cx.notify();
    });
    let task = match start_install(this, root, newest, cx) {
        Ok(task) => task,
        Err(err) => return Err(err),
    };
    let installed = match task.await {
        Ok(installed) => installed,
        Err(err) => return Err(Text::new("ps.play.package.unavailable").text("error", err)),
    };
    // Read again, since the user may have chosen a version while it downloaded: that one runs.
    if let Some(chosen) = settings_now(root, cx).await.selected
        && package::installed_version(root, &chosen).await.is_some()
    {
        return Ok(package::dir(root, &chosen));
    }
    if let Err(err) = select(this, root, installed.version.tag(), false, cx).await {
        return Err(err);
    }
    Ok(package::dir(root, &installed.version))
}
