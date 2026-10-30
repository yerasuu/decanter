//! Secrets to a child process without its environment or arguments (design 3.4): a one-use file
//! only its user may read, which the child removes once read and decanter removes when the child
//! is gone. The environment travels to every wrapper of a launch and to whatever they start, and
//! stays readable in `/proc/<pid>/environ` for the whole run; a path to a removed file does not.

use std::env;
use std::fs::{self, DirBuilder, OpenOptions};
use std::io::{self, Write as _};
use std::os::unix::fs::{DirBuilderExt as _, MetadataExt as _, OpenOptionsExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, SystemTime};

use uuid::Uuid;

use super::Secret;

/// Group and other bits: none may be set on the folder or the file.
const SHARED_BITS: u32 = 0o077;
/// Older than this, a file is one whose child never started or never read it: a child reads its
/// file as it starts, seconds after the write, and the launch that wrote it removes it at the end.
const STALE_AFTER: Duration = Duration::from_mins(10);

static IS_SWEPT: AtomicBool = AtomicBool::new(false);

/// A file holding secrets for one launch, removed when this is dropped. The child should have
/// removed it already; this is the removal on every other path (a spawn that failed, a child that
/// never read it, a launch given up before its spawn).
#[derive(Debug)]
pub struct SecretsFile {
    path: PathBuf,
}

impl SecretsFile {
    /// `text` in a new file of its own, mode `0600`, in `$XDG_RUNTIME_DIR/decanter/secrets`
    /// (a tmpfs that goes at logout), else in `secrets` under decanter's config folder; never in
    /// the library, which travels with backups and syncs, nor in `/tmp`, which other users share.
    /// The folder is `0700`. Files an earlier run left there, ended before it could remove them,
    /// are removed once per process first.
    ///
    /// # Errors
    /// There is no folder to put it in, the folder is not this user's alone, or writing failed.
    /// Nothing is left on disk then.
    #[inline]
    pub fn write(text: &Secret) -> io::Result<Self> {
        let dir = private_dir()?;
        if !IS_SWEPT.swap(true, Ordering::Relaxed) {
            sweep(&dir);
        }
        let path = dir.join(Uuid::new_v4().simple().to_string());
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)?;
        let this = Self { path };
        let dir_owner = fs::symlink_metadata(&dir)?.uid();
        if file.metadata()?.uid() != dir_owner {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!("{} belongs to another user", dir.display()),
            ));
        }
        file.write_all(text.expose().as_bytes())?;
        Ok(this)
    }

    #[inline]
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for SecretsFile {
    #[inline]
    fn drop(&mut self) {
        match fs::remove_file(&self.path) {
            Ok(()) => tracing::info!(
                path = %self.path.display(),
                "secrets file removed by decanter: the child had not removed it"
            ),
            Err(err) if err.kind() == io::ErrorKind::NotFound => {
                tracing::debug!(path = %self.path.display(), "secrets file already removed by the child");
            }
            Err(err) => tracing::warn!(path = %self.path.display(), %err, "could not remove a secrets file"),
        }
    }
}

/// The folder secrets files go in, made `0700` if it is not yet, and refused when it is a link or
/// cannot be made so.
fn private_dir() -> io::Result<PathBuf> {
    let runtime = env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .filter(|dir| dir.is_absolute() && dir.is_dir())
        .map(|dir| dir.join("decanter"));
    let Some(parent) = runtime.or_else(crate::platform::config_dir) else {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "neither XDG_RUNTIME_DIR nor a config folder to put a secrets file in",
        ));
    };
    fs::create_dir_all(&parent)?;
    let dir = parent.join("secrets");
    match DirBuilder::new().mode(0o700).create(&dir) {
        Ok(()) => {}
        Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {}
        Err(err) => return Err(err),
    }
    let metadata = fs::symlink_metadata(&dir)?;
    if !metadata.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{} is not a folder", dir.display()),
        ));
    }
    if metadata.mode() & SHARED_BITS != 0 {
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o700))?;
        tracing::warn!(dir = %dir.display(), "secrets folder was open to other users, made 0700");
    }
    Ok(dir)
}

/// What a run that ended before removing them left behind: only files past [`STALE_AFTER`], since
/// another decanter of the same session may have just written one for its own launch.
fn sweep(dir: &Path) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let now = SystemTime::now();
    let is_stale = |metadata: fs::Metadata| {
        metadata.is_file()
            && metadata
                .modified()
                .is_ok_and(|modified| now.duration_since(modified).is_ok_and(|age| age > STALE_AFTER))
    };
    let removed = entries
        .flatten()
        .filter(|entry| entry.metadata().is_ok_and(is_stale))
        .filter(|entry| fs::remove_file(entry.path()).is_ok())
        .count();
    if removed > 0 {
        tracing::info!(dir = %dir.display(), removed, "removed secrets files an earlier run left");
    }
}
