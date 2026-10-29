use std::io;
use std::path::{Path, PathBuf};

use crate::{MAX_MOD_BYTES, MAX_MOD_FILES};

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    #[error("{}: {source}", path.display())]
    Io { path: PathBuf, source: io::Error },
    #[error("{}: {reason}", path.display())]
    Malformed { path: PathBuf, reason: String },
    #[error("{0:?} is not a path a mod may use")]
    InvalidPath(String),
    #[error("{0:?} is not a mod id")]
    InvalidId(String),
    #[error("{0:?} can't name a folder")]
    InvalidName(String),
    #[error("{0} is not inside a library's common folder")]
    NotAnInstall(String),
    #[error("{0:?} is kept for decanter's Galaxy hook and can't come from a mod")]
    Reserved(String),
    #[error("{first:?} and {second:?} are the same file for a Windows game")]
    Collision { first: String, second: String },
    #[error("{}: only files and folders can be part of a mod, not links or devices", .0.display())]
    NotAFile(PathBuf),
    #[error("{}: points outside its mod", .0.display())]
    Escapes(PathBuf),
    #[error("the folder holds no files")]
    Empty,
    #[error("a mod holds at most {MAX_MOD_FILES} files")]
    TooManyFiles,
    #[error("a mod holds at most {} GiB", MAX_MOD_BYTES >> 30)]
    TooLarge,
    #[error("the mod needs {needed} bytes and the library's disk has {free} free")]
    NoSpace { needed: u64, free: u64 },
    #[error("the folder is inside the game's own mods folder")]
    SourceInsideMods,
    #[error("no mod {0} for this game")]
    UnknownMod(String),
}

impl Error {
    pub(crate) fn io(path: &Path) -> impl FnOnce(io::Error) -> Self + '_ {
        move |source| Self::Io {
            path: path.to_path_buf(),
            source,
        }
    }

    pub(crate) fn malformed(path: &Path, reason: &dyn core::fmt::Display) -> Self {
        Self::Malformed {
            path: path.to_path_buf(),
            reason: reason.to_string(),
        }
    }
}
