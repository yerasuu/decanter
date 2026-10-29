//! The spellings of the port's interface. psport renamed bbport's `BB_*` variables to `PS_*`
//! and `out/bb-probe` to `out/psport`, and later `out/bb-gpu-capabilities` and `bbport.ini` to
//! `out/psport-gpu-capabilities` and `psport.ini`; its releases before each step, which the stable
//! channel can still run, know only the old ones. Every variable decanter passes is written as its
//! suffix and spelled here, so they never drift apart. psport later removed 0.3's memory model
//! with its variables, which [`single_memory_model`] tells.

use std::path::Path;

use tokio::process::Command;

use crate::os;

#[derive(Clone, Copy)]
pub enum Names {
    Psport,
    /// psport with `PS_*` and `out/psport`, still with bbport's file names.
    EarlyPsport,
    Bbport,
}

impl Names {
    pub const ALL: [Self; 3] = [Self::Psport, Self::EarlyPsport, Self::Bbport];

    /// Told apart by `run.sh`, the script that reads the variables, rather than by the binary
    /// under `out/`: `run.sh` is in a checkout and in a fetched build alike and always from the
    /// commit that will run, while `out/` can keep a binary built from another commit after the
    /// channel moved the checkout to an older tag. Unreadable reads as the older port, whose probe
    /// [`Names::probe`] then finds missing.
    pub async fn of(port_dir: &Path) -> Self {
        Self::of_script(tokio::fs::read_to_string(port_dir.join("run.sh")).await.ok().as_deref())
    }

    /// [`Names::of`] for callers already doing file I/O off the UI thread.
    pub fn of_blocking(port_dir: &Path) -> Self {
        Self::of_script(std::fs::read_to_string(port_dir.join("run.sh")).ok().as_deref())
    }

    /// A direct launch has no `run.sh` to tell by: its builds all come after both renames.
    fn of_script(script: Option<&str>) -> Self {
        match script {
            _ if os::DIRECT_LAUNCH => Self::Psport,
            Some(script) if script.contains("psport.ini") => Self::Psport,
            Some(script) if script.contains("PS_PROBE") => Self::EarlyPsport,
            _ => Self::Bbport,
        }
    }

    /// What `launch_command` chose for `command`, for variables added to it afterwards.
    pub fn of_command(command: &Command) -> Self {
        let psport = Self::Psport.var("PROBE");
        // A direct launch runs no script, so nothing sets PS_PROBE to tell it by.
        if os::DIRECT_LAUNCH || command.as_std().get_envs().any(|(key, _)| key == psport.as_str()) {
            Self::Psport
        } else {
            Self::Bbport
        }
    }

    pub const fn prefix(self) -> &'static str {
        match self {
            Self::Psport | Self::EarlyPsport => "PS_",
            Self::Bbport => "BB_",
        }
    }

    pub fn var(self, suffix: &str) -> String {
        format!("{}{suffix}", self.prefix())
    }

    pub const fn probe(self) -> &'static str {
        match self {
            Self::Psport | Self::EarlyPsport => "out/psport",
            Self::Bbport => "out/bb-probe",
        }
    }

    pub const fn gpu_capabilities(self) -> &'static str {
        match self {
            Self::Psport => "out/psport-gpu-capabilities",
            Self::EarlyPsport | Self::Bbport => "out/bb-gpu-capabilities",
        }
    }

    pub const fn ini(self) -> &'static str {
        match self {
            Self::Psport => "psport.ini",
            Self::EarlyPsport | Self::Bbport => "bbport.ini",
        }
    }
}

/// Launch settings of 0.3's memory model, which a port with [`single_memory_model`] ignores: the
/// settings view leaves them out and the launch doesn't pass them.
pub const MEMORY_MODEL_SETTINGS: [&str; 3] = ["pc_model", "preupload", "as_0_3"];

/// Whether the port at `port_dir` has only the PC memory model, told by `run.sh` as [`Names::of`]
/// tells the generation: from the removal on, it warns that it ignores the variables of the old one.
pub async fn single_memory_model(port_dir: &Path) -> bool {
    is_single_memory_model(tokio::fs::read_to_string(port_dir.join("run.sh")).await.ok().as_deref())
}

/// [`single_memory_model`] for callers already doing file I/O off the UI thread.
pub fn single_memory_model_blocking(port_dir: &Path) -> bool {
    is_single_memory_model(std::fs::read_to_string(port_dir.join("run.sh")).ok().as_deref())
}

/// A direct launch's builds all come after the removal, with no `run.sh` to tell by.
fn is_single_memory_model(script: Option<&str>) -> bool {
    os::DIRECT_LAUNCH || script.is_some_and(|text| text.contains("Memory model: the PC one, the only one now"))
}
