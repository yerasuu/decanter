//! `decanter-cli.exe`, the console twin of the GUI-subsystem `decanter.exe` on Windows.
//!
//! A GUI-subsystem program is started without a console, so its own CLI output never reaches
//! the terminal that ran it. This one is a console program that runs the sibling `decanter.exe`
//! with the same arguments and the terminal's standard handles, which the child writes to even
//! without a console, and exits with its status. The CLI itself lives only in `decanter`.

use std::process::{Command, ExitCode};

#[expect(
    clippy::print_stderr,
    reason = "this is a console front end: its only way to report a failure is the terminal"
)]
fn main() -> ExitCode {
    let gui = match std::env::current_exe() {
        Ok(path) => path.with_file_name("decanter.exe"),
        Err(err) => {
            eprintln!("cannot locate decanter.exe: {err}");
            return ExitCode::FAILURE;
        }
    };
    match Command::new(&gui).args(std::env::args_os().skip(1)).status() {
        Ok(status) => status
            .code()
            .and_then(|code| u8::try_from(code).ok())
            .map_or(ExitCode::FAILURE, ExitCode::from),
        Err(err) => {
            eprintln!("cannot run {}: {err}", gui.display());
            ExitCode::FAILURE
        }
    }
}
