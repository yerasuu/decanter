//! What every runner shares, whichever crate gets it onto the machine.
//!
//! A runner is a program decanter gets onto the machine and then runs games
//! with; starting a game with it is the launch side, not this.
//!
//! The operations are plain functions of the crate that owns each runner,
//! with the same names and signatures on every system; a system without a
//! way to get one answers [`Status::Unsupported`].

#![expect(
    clippy::implicit_return,
    reason = "directly contradicts needless_return from clippy::all, also enabled"
)]

use std::fmt;
use std::path::PathBuf;

/// Exhaustive on purpose: a new runner has to be handled at every `match`.
#[derive(Clone, Copy, Debug)]
pub enum Runner {
    /// `umu-run`, which runs Windows games under Proton on Linux.
    Umu,
    /// The Proton builds Steam and umu keep, which umu downloads itself at
    /// launch: decanter only lists them.
    Proton,
    /// The one GE-Proton release decanter installs in the library itself, for
    /// what needs a Proton build without Steam (psport's FSR 4.1.1 recording).
    GeProton,
    /// Raven, an early-stage Windows runtime built here from a pinned commit,
    /// only once the player asks for it.
    Raven,
    /// psport, the PS4 compatibility layer, as its release or built here from
    /// its branch.
    Psport,
    /// AnyPS5, the PS5 executable relinker, built here from its branch.
    AnyPs5,
}

/// A release name, checked where it enters (an answer from `GitHub`, a
/// folder name, a pinned constant) to be one plain path component, since it
/// names a file or folder on disk.
#[derive(Clone, Debug)]
pub struct Tag(String);

impl Tag {
    #[must_use]
    #[inline]
    pub fn try_new(text: &str) -> Option<Self> {
        let is_plain =
            !text.is_empty() && text.len() <= 255 && text != "." && text != ".." && !text.contains(['/', '\\', '\0']);
        is_plain.then(|| Self(text.to_owned()))
    }

    #[must_use]
    #[inline]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Tag {
    #[inline]
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A full git commit id, checked where it enters to be 40 lower-case hex
/// digits, since it names a folder on disk and is handed to git.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Commit(String);

impl Commit {
    #[must_use]
    #[inline]
    pub fn try_new(text: &str) -> Option<Self> {
        let is_full_id = text.len() == 40 && text.bytes().all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'));
        is_full_id.then(|| Self(text.to_owned()))
    }

    #[must_use]
    #[inline]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The first 12 digits, as the UI shows a commit.
    #[must_use]
    #[inline]
    pub fn short(&self) -> &str {
        self.0.get(..12).unwrap_or(&self.0)
    }
}

impl fmt::Display for Commit {
    #[inline]
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// What a runner is fixed to.
#[derive(Clone, Debug)]
pub enum Pin {
    Release(Tag),
    /// Built here from this commit of the runner's repository.
    Commit(Commit),
}

impl fmt::Display for Pin {
    #[inline]
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Release(tag) => tag.fmt(f),
            Self::Commit(commit) => commit.fmt(f),
        }
    }
}

/// How what is installed got there.
#[derive(Debug)]
pub enum Origin {
    /// A published release fetched as it is, not built here.
    Download,
    /// Built on this machine from its sources.
    Local,
}

#[derive(Debug)]
pub struct Installed {
    pub pin: Pin,
    pub origin: Origin,
    /// The runner's program file, or the folder that holds it.
    pub path: PathBuf,
}

#[derive(Debug)]
pub enum Status {
    /// This system has no way to get the runner.
    Unsupported,
    Absent,
    /// Sorted by pin, which is as close to a version order as release names get.
    Installed(Vec<Installed>),
}

impl Status {
    /// Empty unless something is installed.
    #[must_use]
    #[inline]
    pub fn installed(&self) -> &[Installed] {
        match self {
            Self::Installed(installed) => installed,
            Self::Unsupported | Self::Absent => &[],
        }
    }
}

#[derive(Debug)]
pub enum Stage {
    Downloading,
    Unpacking,
    /// Files already installed hashed again before they are used: `received`
    /// files of `total`, which is 0 until their lists are read.
    Verifying,
    /// Getting the sources at the pin.
    Sources,
    Configuring,
    /// `received` steps of `total`, as the build tool counts them.
    Compiling,
    /// What was built put together in the shape of a published release.
    Assembling,
}

/// Where a preparation is; the byte counts belong to the stage, and stay at
/// zero for one that moves no bytes.
#[derive(Debug)]
pub struct Progress {
    pub stage: Stage,
    pub received: u64,
    pub total: Option<u64>,
    /// The piece the stage works on, for a preparation that downloads and
    /// unpacks several (a build's toolchain): its name as the UI shows it.
    pub item: Option<String>,
}
