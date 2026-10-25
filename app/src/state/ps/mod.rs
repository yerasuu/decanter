//! The PS section's state: the games in the PS folders of the active
//! profile's library folders.
//!
//! Created the first time the section opens, not at startup, so a PC
//! profile never pays for it; kept after that, so jobs started from the
//! section outlive switching to another one.

mod branch;
mod bundle;
mod channel;
#[cfg(windows)]
mod direct;
mod fallback;
mod fsr;
mod jobs;
mod moving;
mod package;
mod play;
mod ports;
mod vk_layer;

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use decanter_core::capabilities::Feature;
use decanter_ps::bbport::direct::FolderError;
use decanter_ps::library;
use decanter_ps::port::Port;
use decanter_ps::process::Job;
use decanter_ps::status::{self, Entry};
use gpui::{App, AppContext as _, Context, Entity, Global};

use branch::BranchState;
pub use channel::Track;
pub use fallback::Fallback;
pub use fsr::DllOutcome;
pub use jobs::{JobKey, label as job_label, target_name as job_target};
use package::PackageState;
pub use play::{failure_message, online_error};
pub use ports::{BuildOrigin, PortState, Stage};
pub use vk_layer::unavailable_text as vk_layer_unavailable_text;

use super::AppState;
use crate::capabilities::Caps;
use crate::i18n::Text;

pub struct Ps {
    profile_id: Option<String>,
    entries: Vec<Entry>,
    /// The library folders the last scan read, the primary one first.
    library_roots: Vec<PathBuf>,
    scanning: bool,
    error: Option<Text>,
    /// The latest outcome worth telling: `(text, is_error)`.
    notice: Option<(Text, bool)>,
    /// The outcome of each game's latest run by game folder, `(text, is_error)`, so running another game
    /// does not take it off the page it belongs to.
    run_notices: HashMap<PathBuf, (Text, bool)>,
    /// Bumped whenever the profile's failure record changes, for the
    /// view that reads it to know when to read it again.
    failures_revision: u32,
    /// Bumped when an FSR 4.1.1 build ends, so its status is checked again.
    fsr_revision: u32,
    /// Bumped whenever what a run taught about a title changes, for the
    /// game's view to read it again.
    learned_revision: u32,
    /// Bumped per scan, so a slow scan that finishes after a newer one,
    /// or after a profile switch, is dropped instead of shown.
    scan: u32,
    /// The latest job of each kind, finished ones included, for their logs.
    jobs: HashMap<JobKey, Entity<Job>>,
    /// Games whose port is prepared or command built but whose job doesn't
    /// exist yet, so a second JUGAR doesn't start them twice.
    launching: HashSet<PathBuf>,
    /// Games being moved to another library folder, which can't start
    /// until they land.
    moving: HashSet<PathBuf>,
    /// Games whose session is being stopped, so DETENER can't start a second stop meanwhile.
    stopping: HashSet<PathBuf>,
    ports: HashMap<Port, PortState>,
    /// Where `psport.exe` runs directly ([`decanter_ps::bbport::direct::IN_USE`]).
    package: PackageState,
    /// psport's branch built here where `psport.exe` runs directly.
    branch: BranchState,
}

struct PsGlobal(Entity<Ps>);

impl Global for PsGlobal {}

impl Ps {
    pub fn get(cx: &mut App) -> Entity<Self> {
        if let Some(global) = cx.try_global::<PsGlobal>() {
            return global.0.clone();
        }
        let session = AppState::get(cx).session.clone();
        let ps = cx.new(|cx| {
            cx.observe(&session, |this: &mut Self, _, cx| this.sync_profile(cx))
                .detach();
            let mut this = Self {
                profile_id: None,
                entries: Vec::new(),
                library_roots: Vec::new(),
                notice: None,
                run_notices: HashMap::new(),
                scanning: false,
                error: None,
                scan: 0,
                failures_revision: 0,
                fsr_revision: 0,
                learned_revision: 0,
                jobs: HashMap::new(),
                launching: HashSet::new(),
                moving: HashSet::new(),
                stopping: HashSet::new(),
                ports: HashMap::new(),
                package: PackageState::default(),
                branch: BranchState::default(),
            };
            this.sync_profile(cx);
            // Where psport.exe runs directly, builds are the branch's (`branch`), not the checkouts'.
            if !decanter_ps::bbport::direct::IN_USE && Caps::get(cx).is_offered(Feature::PsBuilds) {
                Self::start_update_loop(cx);
            }
            if decanter_ps::bbport::direct::IN_USE && Caps::get(cx).is_offered(Feature::PsDownloads) {
                Self::start_package_loop(cx);
            }
            this
        });
        cx.set_global(PsGlobal(ps.clone()));
        ps
    }

    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    pub fn library_roots(&self) -> &[PathBuf] {
        &self.library_roots
    }

    /// The library folder holding the game in `dir`, and whether it is the
    /// primary one.
    pub fn library_of(&self, dir: &Path) -> Option<(&Path, bool)> {
        self.library_roots
            .iter()
            .enumerate()
            .find(|(_, root)| dir.starts_with(decanter_ps::paths::games_in(root)))
            .map(|(index, root)| (root.as_path(), index == 0))
    }

    pub const fn notice(&self) -> Option<&(Text, bool)> {
        self.notice.as_ref()
    }

    pub fn run_notice(&self, dir: &Path) -> Option<&(Text, bool)> {
        self.run_notices.get(dir)
    }

    pub const fn scanning(&self) -> bool {
        self.scanning
    }

    pub const fn error(&self) -> Option<&Text> {
        self.error.as_ref()
    }

    pub const fn fsr_revision(&self) -> u32 {
        self.fsr_revision
    }

    pub const fn failures_revision(&self) -> u32 {
        self.failures_revision
    }

    /// Where the active profile's failures are kept.
    pub fn failures_path(&self) -> Option<PathBuf> {
        self.profile_id
            .as_deref()
            .map(|profile_id| decanter_ps::paths::active().profile(profile_id).join("failures.jsonl"))
    }

    pub fn failures_changed(&mut self, cx: &mut Context<Self>) {
        self.failures_revision = self.failures_revision.wrapping_add(1);
        cx.notify();
    }

    pub const fn learned_revision(&self) -> u32 {
        self.learned_revision
    }

    pub fn learned_changed(&mut self, cx: &mut Context<Self>) {
        self.learned_revision = self.learned_revision.wrapping_add(1);
        cx.notify();
    }

    pub fn job(&self, key: &JobKey) -> Option<&Entity<Job>> {
        self.jobs.get(key)
    }

    pub fn port(&self, port: Port) -> Option<&PortState> {
        self.ports.get(&port)
    }

    fn sync_profile(&mut self, cx: &mut Context<Self>) {
        let profile_id = AppState::get(cx).session.read(cx).active_profile.clone();
        if profile_id == self.profile_id {
            return;
        }
        self.profile_id = profile_id.clone();
        self.entries.clear();
        self.library_roots.clear();
        self.notice = None;
        self.run_notices.clear();
        self.scan = self.scan.wrapping_add(1);
        self.scanning = false;
        let Some(profile_id) = profile_id else {
            self.rescan(cx);
            return;
        };
        // Everything PS lives in the profile's library, so its root is
        // settled before any scan, import or port reads a path.
        let lookup = cx.background_spawn({
            let profile_id = profile_id.clone();
            async move { crate::core::library_for_profile(&profile_id).map(|library| library.root().to_owned()) }
        });
        cx.spawn(async move |this, cx| {
            let root = lookup.await;
            let _ = this.update(cx, |this, cx| {
                if this.profile_id.as_ref() != Some(&profile_id) {
                    return;
                }
                match root {
                    Ok(root) => {
                        decanter_ps::paths::use_library(&root);
                        this.rescan(cx);
                    }
                    Err(err) => {
                        tracing::warn!(%err, "could not place the PS data in the profile's library");
                        this.error = Some(Text::new("ps.games.library_error").arg("error", err));
                        cx.notify();
                    }
                }
            });
        })
        .detach();
    }

    /// Reads the folder list and every dump in it again, off the UI thread.
    pub fn rescan(&mut self, cx: &mut Context<Self>) {
        self.scan = self.scan.wrapping_add(1);
        let Some(profile_id) = self.profile_id.clone() else {
            self.scanning = false;
            cx.notify();
            return;
        };
        let scan = self.scan;
        self.scanning = true;
        cx.notify();
        let root = decanter_ps::paths::active();
        let task = cx.background_spawn(async move {
            // Read again on every scan, since a folder can be added or its
            // disk plugged in while the section is open.
            let library_roots = crate::core::library_for_profile(&profile_id).map_or_else(
                |err| {
                    tracing::warn!(profile_id, %err, "could not list the library folders, reading the primary one only");
                    vec![root.library().to_owned()]
                },
                |library| library.roots(),
            );
            // Only the primary folder is made if missing, so there is a
            // place to drop dumps in; an extra one gets its folder when a
            // game is moved there.
            let games = root.games();
            if let Err(err) = std::fs::create_dir_all(&games) {
                return Err(Text::new("ps.errors.create_dir").arg("dir", games.display()).arg("error", err));
            }
            let folders: Vec<PathBuf> = std::iter::once(games)
                .chain(
                    library_roots
                        .iter()
                        .skip(1)
                        .map(|library_root| decanter_ps::paths::games_in(library_root))
                        .filter(|folder| folder.is_dir()),
                )
                .collect();
            Ok((status::classify(&root, &library::scan(&folders)), library_roots))
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                if this.scan != scan {
                    return;
                }
                this.scanning = false;
                match result {
                    Ok((entries, library_roots)) => {
                        tracing::debug!(
                            games = entries.len(),
                            folders = library_roots.len(),
                            "scanned the PS games folders"
                        );
                        // New or changed dumps (a copy dropped in, a merge) are measured now, off
                        // the UI thread, so Almacenamiento only has to stat them.
                        let dirs: Vec<PathBuf> = entries.iter().map(|entry| entry.game.dir.clone()).collect();
                        cx.background_spawn(
                            async move { crate::dump_size::refresh(dirs.iter().map(PathBuf::as_path)) },
                        )
                        .detach();
                        this.entries = entries;
                        this.library_roots = library_roots;
                        this.error = None;
                    }
                    Err(err) => {
                        tracing::warn!(%err, "could not read the PS games folder");
                        this.error = Some(err);
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }
}

/// Why a folder picked for `psport.exe` can't be used, in the UI's language.
pub fn folder_error(err: &FolderError) -> Text {
    let (key, dir) = match err {
        FolderError::Linked { dir, .. } => ("ps.settings.psport_folder.errors.linked", dir),
        FolderError::Missing { dir, .. } => ("ps.settings.psport_folder.errors.missing", dir),
        FolderError::NotAFolder { dir, .. } => ("ps.settings.psport_folder.errors.not_a_folder", dir),
        FolderError::NotLocal(dir) => ("ps.settings.psport_folder.errors.not_local", dir),
        FolderError::Relative(dir) => ("ps.settings.psport_folder.errors.relative", dir),
    };
    let text = Text::new(key).arg("dir", dir.display());
    match err {
        FolderError::Linked { file, .. } | FolderError::Missing { file, .. } => text.arg("file", file),
        FolderError::NotAFolder { err: cause, .. } => text.arg("error", cause),
        FolderError::NotLocal(_) | FolderError::Relative(_) => text,
    }
}
