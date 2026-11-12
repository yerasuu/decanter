//! The command a start from outside decanter runs (a task bar entry, a Steam shortcut): decanter's
//! own CLI, built here once so every one of them starts a game the same way.

use std::path::{Path, PathBuf};
use std::sync::Once;

use decanter_core::steam;

/// The runtime option that makes an `AppImage` extract itself and run from the copy instead of
/// mounting itself with FUSE ([`runtime.c`]).
///
/// [`runtime.c`]: https://github.com/AppImage/type2-runtime/blob/20251108/src/runtime/runtime.c
const APPIMAGE_EXTRACT_AND_RUN: &str = "--appimage-extract-and-run";

/// `decanter [--library <root>] <command>`.
pub struct Launcher {
    /// A decanter binary, never a dynamic loader.
    pub program: PathBuf,
    pub args: Vec<String>,
    pub is_appimage: bool,
}

impl Launcher {
    /// `launch` for an installed GOG game.
    pub fn gog(account_id: &str, product_id: &str) -> Option<Self> {
        Self::new(&["launch", "--account-id", account_id, "--product-id", product_id])
    }

    /// `ps play` for a PS game: as the window's Play, with its own settings, online play included.
    pub fn ps(title_id: &str) -> Option<Self> {
        Self::new(&["ps", "play", "--title", title_id])
    }

    fn new(command: &[&str]) -> Option<Self> {
        let (program, is_appimage) = binary()?;
        let mut args = Vec::with_capacity(command.len() + 2);
        if let Some(root) = library() {
            args.push("--library".to_owned());
            args.push(root.to_str()?.to_owned());
        }
        args.extend(command.iter().map(|arg| (*arg).to_owned()));
        Some(Self {
            program,
            args,
            is_appimage,
        })
    }

    /// The shortcut Steam runs; `None`, warned, when an argument or the program's path holds what
    /// Steam's launch options line would read differently. Valve documents no escaping for that
    /// line, which goes through `/bin/sh -c` on Linux, so such a value is refused rather than
    /// escaped in a way Steam may not read back.
    pub fn steam(&self) -> Option<steam::Command> {
        let program = self.program.to_str().filter(|path| is_plain(path));
        // Steam starts a shortcut from its runtime's launcher service, where the AppImage runtime
        // finds no fusermount on PATH and can't mount itself, so the AppImage is told to extract
        // and run instead. A task bar entry runs in the desktop session, where FUSE works.
        let extract = self.is_appimage.then_some(APPIMAGE_EXTRACT_AND_RUN);
        let options = extract
            .into_iter()
            .chain(self.args.iter().map(String::as_str))
            .map(|arg| is_plain(arg).then(|| steam_arg(arg)))
            .collect::<Option<Vec<_>>>();
        let (Some(_), Some(options), Some(start_dir)) = (program, options, self.program.parent()) else {
            tracing::warn!(
                program = %self.program.display(),
                "no Steam shortcut written, its command can't be put in Steam's launch options"
            );
            return None;
        };
        Some(steam::Command {
            exe: self.program.clone(),
            // What Steam's own "Add a Non-Steam Game" writes: the program's folder. The CLI picks
            // each game's own working directory itself.
            start_dir: start_dir.to_owned(),
            launch_options: options.join(" "),
        })
    }
}

/// Nothing that would end a double-quoted shell word or expand inside one. A backslash passes only on
/// Windows, since its paths are written with it and no shell reads the line there.
fn is_plain(arg: &str) -> bool {
    !arg.chars()
        .any(|ch| matches!(ch, '"' | '$' | '`') || (cfg!(not(windows)) && ch == '\\') || ch.is_control())
}

/// `arg` as one word of Steam's launch options, quoted only when it holds more than an id or a plain
/// path does.
fn steam_arg(arg: &str) -> String {
    let is_bare = !arg.is_empty()
        && arg
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.' | '/' | ':' | '=' | '+' | ','));
    if is_bare { arg.to_owned() } else { format!("\"{arg}\"") }
}

/// The decanter binary a start from outside runs, and whether it is the `AppImage`: the `AppImage`
/// when decanter runs as one, since the binary it runs from is inside a mount (or an extracted copy)
/// that goes away with it, else the one `DECANTER_LAUNCHER` names,
/// else this binary. `None` when this one was started through a dynamic loader by hand
/// (`<ld.so> --library-path <nix libraries> target/debug/decanter`, a nix shell's development run):
/// that command line is the development shell's, so it is never written into an entry, and
/// decanter alone fails to start outside that shell.
pub fn binary() -> Option<(PathBuf, bool)> {
    let named = |key| {
        std::env::var_os(key)
            .map(PathBuf::from)
            .filter(|path| path.is_absolute() && path.is_file() && !is_loader(path))
    };
    if let Some(appimage) = named("APPIMAGE") {
        return Some((appimage, true));
    }
    if let Some(launcher) = named("DECANTER_LAUNCHER") {
        return Some((launcher, false));
    }
    let exe = std::env::current_exe().ok()?;
    if !is_loader(&exe) {
        return Some((exe, false));
    }
    static WARNED: Once = Once::new();
    WARNED.call_once(|| {
        tracing::warn!(
            loader = %exe.display(),
            "decanter runs through a dynamic loader, no start from outside decanter is written; set DECANTER_LAUNCHER to a decanter binary"
        );
    });
    None
}

/// Whether `command` was written for the `AppImage` decanter runs as.
pub fn starts_appimage(command: &steam::Command) -> bool {
    command.launch_options.split(' ').next() == Some(APPIMAGE_EXTRACT_AND_RUN)
}

/// Whether `exe`, the `Exe` field of a shortcut with its quotes, names a decanter binary (an
/// `AppImage` or the executable, however it was renamed) rather than a game's own program or a
/// dynamic loader.
pub fn names_decanter(exe: &str) -> bool {
    let path = Path::new(exe.trim_matches('"'));
    !is_loader(path)
        && path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.to_ascii_lowercase().starts_with("decanter"))
}

fn is_loader(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.starts_with("ld-linux") || name.starts_with("ld.so") || name.starts_with("ld64.so"))
}

/// The library a start from outside names, so the game starts from it whichever one is active.
fn library() -> Option<PathBuf> {
    match decanter_core::bootstrap::location() {
        decanter_core::bootstrap::Location::Library(root) => Some(root),
        decanter_core::bootstrap::Location::Legacy(_) | decanter_core::bootstrap::Location::Unconfigured => None,
    }
}
