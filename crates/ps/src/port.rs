//! The two ports decanter runs, and where their checkouts live.

use std::path::PathBuf;

use crate::paths::Root;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Port {
    Bbport,
    AnyPs5,
}

impl Port {
    pub const ALL: [Self; 2] = [Self::Bbport, Self::AnyPs5];

    pub const fn name(self) -> &'static str {
        match self {
            Self::Bbport => "bbport",
            Self::AnyPs5 => "AnyPS5",
        }
    }

    /// Followed on its default branch: each port moves fast and decanter
    /// runs whatever upstream ships.
    pub const fn upstream(self) -> &'static str {
        match self {
            // decanter's fork of deadinside28/bloodborne_pc: other PS4 titles on top of upstream.
            Self::Bbport => "https://github.com/yerasuu/psport.git",
            Self::AnyPs5 => "https://github.com/boykopovar/AnyPS5.git",
        }
    }

    pub fn checkout(self, root: &Root) -> PathBuf {
        let dir = match self {
            Self::Bbport => "bbport",
            Self::AnyPs5 => "anyps5",
        };
        root.join("ports").join(dir)
    }
}
