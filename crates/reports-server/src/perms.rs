//! Owner-only files: the local driver's directory and the files holding secrets must not be readable by other local
//! users.

use std::fs;
use std::io;
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::path::Path;

use rustix::fs::Mode;

const GROUP_AND_OTHER: u32 = 0o077;

/// Files and directories the process creates from here on carry no group or other bits, the local driver's database
/// file included.
pub fn restrict_umask() {
    rustix::process::umask(Mode::from_raw_mode(GROUP_AND_OTHER));
}

/// Narrows `path` to `mode` when it grants anything to group or others; reports whether it had to.
fn tighten(path: &Path, mode: u32) -> io::Result<bool> {
    let current = fs::metadata(path)?.permissions().mode();
    if current & GROUP_AND_OTHER == 0 {
        return Ok(false);
    }
    fs::set_permissions(path, fs::Permissions::from_mode(mode))?;
    tracing::warn!(path = %path.display(), was = format_args!("{:04o}", current & 0o7777), "loose permissions narrowed");
    Ok(true)
}

/// Creates the directory (and missing parents) with mode 0700, and narrows an existing one that is looser.
///
/// # Errors
///
/// The directory cannot be created or its permissions cannot be read or changed.
pub fn private_dir(path: &Path) -> io::Result<()> {
    fs::DirBuilder::new().recursive(true).mode(0o700).create(path)?;
    tighten(path, 0o700).map(drop)
}

/// Whether the open file grants anything to group or others.
///
/// # Errors
///
/// The metadata cannot be read.
pub fn is_shared(file: &fs::File) -> io::Result<Option<u32>> {
    let mode = file.metadata()?.permissions().mode() & 0o7777;
    Ok((mode & GROUP_AND_OTHER != 0).then_some(mode))
}

#[cfg(test)]
mod tests {
    use std::error::Error;

    use super::*;

    type Test = Result<(), Box<dyn Error>>;

    fn mode_of(path: &Path) -> Result<u32, io::Error> {
        Ok(fs::metadata(path)?.permissions().mode() & 0o7777)
    }

    fn scratch() -> Result<std::path::PathBuf, Box<dyn Error>> {
        Ok(std::env::temp_dir().join(format!("decanter-reports-perms-{}", crate::secrets::random_hex()?)))
    }

    #[test]
    fn new_directory_is_0700_and_a_loose_one_is_narrowed() -> Test {
        let root = scratch()?;
        private_dir(&root.join("a/b"))?;
        assert_eq!(mode_of(&root.join("a/b"))?, 0o700);
        fs::set_permissions(&root, fs::Permissions::from_mode(0o755))?;
        private_dir(&root)?;
        assert_eq!(mode_of(&root)?, 0o700);
        fs::remove_dir_all(&root)?;
        Ok(())
    }
}
