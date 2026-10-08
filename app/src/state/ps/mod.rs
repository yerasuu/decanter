//! The PS section's state: the active PS profile's folders and the games
//! found in them.
//!
//! Created the first time the section opens, not at startup, so a PC
//! profile never pays for it; kept after that, so jobs started from the
//! section outlive switching to another one.

mod fsr;
mod jobs;
mod play;
mod ports;

use std::collections::HashMap;
use std::path::PathBuf;

use decanter_core::ProfileKind;
use decanter_ps::library;
use decanter_ps::port::Port;
use decanter_ps::process::Job;
use decanter_ps::status::{self, Entry};
use gpui::{App, AppContext as _, Context, Entity, Global};

pub use fsr::DllOutcome;
pub use jobs::JobKey;
pub use ports::{PortState, Stage};

use super::AppState;

pub struct Ps {
    profile_id: Option<String>,
    dirs: Vec<String>,
    entries: Vec<Entry>,
    scanning: bool,
    error: Option<String>,
    /// The latest outcome worth telling: `(text, is_error)`.
    notice: Option<(String, bool)>,
    /// Bumped whenever the profile's failure record changes, for the
    /// view that reads it to know when to read it again.
    failures_revision: u32,
    /// Bumped when an FSR 4.1.1 build ends, so its status is checked again.
    fsr_revision: u32,
    /// Bumped per scan, so a slow scan that finishes after a newer one,
    /// or after a profile switch, is dropped instead of shown.
    scan: u32,
    /// The latest job of each kind, finished ones included, for their logs.
    jobs: HashMap<JobKey, Entity<Job>>,
    ports: HashMap<Port, PortState>,
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
                dirs: Vec::new(),
                entries: Vec::new(),
                notice: None,
                scanning: false,
                error: None,
                scan: 0,
                failures_revision: 0,
                fsr_revision: 0,
                jobs: HashMap::new(),
                ports: HashMap::new(),
            };
            this.sync_profile(cx);
            Self::start_update_loop(cx);
            this
        });
        cx.set_global(PsGlobal(ps.clone()));
        ps
    }

    pub fn dirs(&self) -> &[String] {
        &self.dirs
    }

    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    pub fn notice(&self) -> Option<&(String, bool)> {
        self.notice.as_ref()
    }

    pub const fn scanning(&self) -> bool {
        self.scanning
    }

    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    pub const fn fsr_revision(&self) -> u32 {
        self.fsr_revision
    }

    pub const fn failures_revision(&self) -> u32 {
        self.failures_revision
    }

    /// Where the active profile's failures are kept, when a PS profile is active.
    pub fn failures_path(&self) -> Option<PathBuf> {
        self.profile_id
            .as_deref()
            .map(|profile_id| decanter_ps::paths::profile(profile_id).join("failures.jsonl"))
    }

    pub fn failures_changed(&mut self, cx: &mut Context<Self>) {
        self.failures_revision = self.failures_revision.wrapping_add(1);
        cx.notify();
    }

    pub fn job(&self, key: &JobKey) -> Option<&Entity<Job>> {
        self.jobs.get(key)
    }

    pub fn port(&self, port: Port) -> Option<&PortState> {
        self.ports.get(&port)
    }

    fn sync_profile(&mut self, cx: &mut Context<Self>) {
        let session = AppState::get(cx).session.read(cx);
        let profile_id = (session.active_kind == ProfileKind::Ps)
            .then(|| session.active_profile.clone())
            .flatten();
        if profile_id == self.profile_id {
            return;
        }
        self.profile_id = profile_id;
        self.dirs.clear();
        self.entries.clear();
        self.notice = None;
        self.rescan(cx);
        self.import_psport(cx);
    }

    /// What the standalone psport left behind, brought into the first PS
    /// profile that opens the section; once per database.
    fn import_psport(&self, cx: &mut Context<Self>) {
        const DONE: &str = "psport_import_v1";
        let Some(profile_id) = self.profile_id.clone() else {
            return;
        };
        let task = cx.background_spawn(async move {
            let db = match crate::core::db() {
                Ok(db) => db,
                Err(err) => return Err(err),
            };
            if db.setting(DONE).map_err(|err| err.to_string())?.is_some() {
                return Ok(false);
            }
            let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
            let sources = decanter_ps::import::Sources::from_home(&home);
            let imported = decanter_ps::import::import(&sources, &profile_id).map_err(|err| err.to_string())?;
            for dir in &imported.scan_dirs {
                db.add_ps_scan_dir(&profile_id, &dir.to_string_lossy())
                    .map_err(|err| err.to_string())?;
            }
            db.set_setting(DONE, "1").map_err(|err| err.to_string())?;
            tracing::info!(
                profile_id,
                folders = imported.scan_dirs.len(),
                copied = ?imported.copied,
                "imported psport's data"
            );
            Ok(!imported.scan_dirs.is_empty() || !imported.copied.is_empty())
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| match result {
                Ok(true) => {
                    this.notice = Some(("Se importaron los datos de psport".to_owned(), false));
                    this.rescan(cx);
                }
                Ok(false) => {}
                Err(err) => {
                    tracing::warn!(%err, "could not import psport's data");
                    this.notice = Some((format!("No se pudieron importar los datos de psport: {err}"), true));
                    cx.notify();
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
        let task = cx.background_spawn(async move {
            let dirs =
                match crate::core::db().and_then(|db| db.ps_scan_dirs(&profile_id).map_err(|err| err.to_string())) {
                    Ok(dirs) => dirs,
                    Err(err) => return Err(err),
                };
            let roots: Vec<PathBuf> = dirs.iter().map(PathBuf::from).collect();
            let entries = status::classify(&library::scan(&roots));
            Ok((dirs, entries))
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                if this.scan != scan {
                    return;
                }
                this.scanning = false;
                match result {
                    Ok((dirs, entries)) => {
                        tracing::debug!(folders = dirs.len(), games = entries.len(), "scanned PS folders");
                        this.dirs = dirs;
                        this.entries = entries;
                        this.error = None;
                    }
                    Err(err) => {
                        tracing::warn!(%err, "could not read the PS folders");
                        this.error = Some(err);
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub fn add_dir(&mut self, path: String, cx: &mut Context<Self>) {
        self.edit_dirs(path, true, cx);
    }

    pub fn remove_dir(&mut self, path: String, cx: &mut Context<Self>) {
        self.edit_dirs(path, false, cx);
    }

    fn edit_dirs(&mut self, path: String, add: bool, cx: &mut Context<Self>) {
        let Some(profile_id) = self.profile_id.clone() else {
            return;
        };
        let task = cx.background_spawn(async move {
            crate::core::db().and_then(|db| {
                if add {
                    db.add_ps_scan_dir(&profile_id, &path)
                } else {
                    db.remove_ps_scan_dir(&profile_id, &path)
                }
                .map_err(|err| err.to_string())
            })
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| match result {
                Ok(()) => this.rescan(cx),
                Err(err) => {
                    tracing::warn!(%err, "could not change the PS folders");
                    this.error = Some(err);
                    cx.notify();
                }
            });
        })
        .detach();
    }
}
