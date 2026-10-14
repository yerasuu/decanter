//! The two spellings of the port's interface. psport renamed bbport's `BB_*` variables to `PS_*`
//! and `out/bb-probe` to `out/psport`; its releases before that, which the stable channel can
//! still run, know only the old ones. Every variable decanter passes is written as its suffix and
//! spelled here, so the two never drift apart.

use std::path::Path;

use tokio::process::Command;

#[derive(Clone, Copy)]
pub enum Names {
    Psport,
    Bbport,
}

impl Names {
    pub const ALL: [Self; 2] = [Self::Psport, Self::Bbport];

    /// Told apart by `run.sh`, the script that reads the variables, rather than by the binary
    /// under `out/`: `run.sh` is in a checkout and in a fetched build alike and always from the
    /// commit that will run, while `out/` can keep a binary built from another commit after the
    /// channel moved the checkout to an older tag. Unreadable reads as the older port, whose probe
    /// [`Names::probe`] then finds missing.
    pub async fn of(port_dir: &Path) -> Self {
        match tokio::fs::read_to_string(port_dir.join("run.sh")).await {
            Ok(script) if script.contains("PS_PROBE") => Self::Psport,
            _ => Self::Bbport,
        }
    }

    /// What `launch_command` chose for `command`, for variables added to it afterwards.
    pub fn of_command(command: &Command) -> Self {
        let psport = Self::Psport.var("PROBE");
        if command.as_std().get_envs().any(|(key, _)| key == psport.as_str()) {
            Self::Psport
        } else {
            Self::Bbport
        }
    }

    pub const fn prefix(self) -> &'static str {
        match self {
            Self::Psport => "PS_",
            Self::Bbport => "BB_",
        }
    }

    pub fn var(self, suffix: &str) -> String {
        format!("{}{suffix}", self.prefix())
    }

    pub const fn probe(self) -> &'static str {
        match self {
            Self::Psport => "out/psport",
            Self::Bbport => "out/bb-probe",
        }
    }
}
