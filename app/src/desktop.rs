//! Handing a path or URL to the desktop's own opener.

use std::ffi::OsStr;
use std::io;
use std::process::Stdio;

/// `open::that_detached`, but with decanter's own environment stripped
/// (see [`decanter_core::strip_launcher_env`]): a browser that starts
/// fresh for the GOG login would otherwise run without GPU acceleration
/// until it's restarted.
///
/// Its own process group, so the opened app doesn't share decanter's
/// terminal signals; reaped on a thread of its own rather than left a
/// zombie, since `xdg-open` and friends exit as soon as they've handed off.
///
/// # Errors
/// The last launcher's spawn error, once every launcher `open` knows of
/// has failed to start.
pub fn open(target: impl AsRef<OsStr>) -> io::Result<()> {
    let mut last_err = io::Error::new(io::ErrorKind::NotFound, "no launcher to open this with");
    for mut command in open::commands(target) {
        decanter_core::strip_launcher_env(&mut command);
        command.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
        #[cfg(unix)]
        std::os::unix::process::CommandExt::process_group(&mut command, 0);
        let spawned = command.spawn();
        match spawned {
            Ok(mut child) => {
                std::thread::spawn(move || child.wait());
                return Ok(());
            }
            Err(err) => last_err = err,
        }
    }
    Err(last_err)
}
