//! The PS section's state: the games in the active profile's library PS
//! folder.
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

    /// Where the active profile's failures are kept.
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
        let profile_id = AppState::get(cx).session.read(cx).active_profile.clone();
        if profile_id == self.profile_id {
            return;
        }
        self.profile_id = profile_id.clone();
        self.entries.clear();
        self.notice = None;
        let Some(profile_id) = profile_id else {
            self.rescan(cx);
            return;
        };
        // Everything PS lives in the profile's library, so its root is
        // settled before any scan, import or port reads a path.
        let settle = cx.background_spawn(async move {
            let library = crate::core::library_for_profile(&profile_id)?;
            decanter_ps::paths::use_library(library.root());
            Ok::<(), String>(())
        });
        cx.spawn(async move |this, cx| {
            let settled = settle.await;
            let _ = this.update(cx, |this, cx| {
                if let Err(err) = settled {
                    tracing::warn!(%err, "could not place the PS data in the profile's library");
                    this.error = Some(format!("No se pudo usar la carpeta de la biblioteca: {err}"));
                }
                this.rescan(cx);
            });
        })
        .detach();
    }

    /// Reads the folder list and every dump in it again, off the UI thread.
    pub fn rescan(&mut self, cx: &mut Context<Self>) {
        self.scan = self.scan.wrapping_add(1);
        if self.profile_id.is_none() {
            self.scanning = false;
            cx.notify();
            return;
        }
        let scan = self.scan;
        self.scanning = true;
        cx.notify();
        // The library's own folder, made if missing so there is a place to
        // drop dumps in.
        let task = cx.background_spawn(async move {
            let games = decanter_ps::paths::games();
            if let Err(err) = std::fs::create_dir_all(&games) {
                return Err(format!("No se pudo crear {}: {err}", games.display()));
            }
            Ok(status::classify(&library::scan(std::slice::from_ref(&games))))
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                if this.scan != scan {
                    return;
                }
                this.scanning = false;
                match result {
                    Ok(entries) => {
                        tracing::debug!(games = entries.len(), "scanned the PS games folder");
                        this.entries = entries;
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
