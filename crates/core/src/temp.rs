//! Scratch folders only this user can enter, with names nobody can guess.

use std::env;
use std::fs::DirBuilder;
use std::io;
use std::path::PathBuf;

use uuid::Uuid;

/// A new empty folder for `purpose`, named at random, mode `0700` where the platform has modes.
///
/// It goes under `$XDG_RUNTIME_DIR` when that is set (a tmpfs of this user alone), else under the
/// system temporary folder, so a predictable name in a shared `/tmp` can neither be pre-created by
/// another user nor reveal what the folder is for. The caller removes it.
///
/// # Errors
/// Creating the folder.
#[inline]
pub fn private_temp_dir(purpose: &str) -> io::Result<PathBuf> {
    let base = env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .filter(|dir| dir.is_absolute())
        .unwrap_or_else(env::temp_dir);
    let dir = base.join(format!("decanter-{purpose}-{}", Uuid::new_v4().simple()));
    #[cfg(unix)]
    let builder = {
        use std::os::unix::fs::DirBuilderExt as _;
        let mut builder = DirBuilder::new();
        builder.mode(0o700);
        builder
    };
    #[cfg(not(unix))]
    let builder = DirBuilder::new();
    builder.create(&dir)?;
    Ok(dir)
}
