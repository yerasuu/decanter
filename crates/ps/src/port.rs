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
            // psport: an independent compatibility layer derived from deadinside28/bloodborne_pc, for any PS4 title.
            Self::Bbport => "https://github.com/yerasuu/psport.git",
            Self::AnyPs5 => "https://github.com/boykopovar/AnyPS5.git",
        }
    }

    /// The Nix binary cache its CI publishes each default-branch build to
    /// ([`crate::prebuilt`]), as a release of the same repository.
    pub const fn binary_cache(self) -> Option<&'static str> {
        match self {
            Self::Bbport => Some("https://github.com/yerasuu/psport/releases/download/nix-cache"),
            Self::AnyPs5 => None,
        }
    }

    /// Where its CI publishes each release's bundle ([`crate::bundle`]), as assets of the tag's own
    /// release: `<this>/<tag>/<asset>`.
    pub const fn release_downloads(self) -> Option<&'static str> {
        match self {
            Self::Bbport => Some("https://github.com/yerasuu/psport/releases/download"),
            Self::AnyPs5 => None,
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
