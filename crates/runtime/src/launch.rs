//! One game launch, described the same way on every system.

use std::path::PathBuf;

use crate::prefix::Prefix;
use crate::proton::Proton;

/// One game launch.
///
/// What only one system's backend reads is plain data here too, so that a
/// launch is built the same way everywhere: the Proton fields (`prefix`,
/// `proton`, `game_id`, `store`, `dll_overrides`) and the Linux ones
/// (`native`, `gamemoded`, `identity`) are ignored on Windows and macOS, and
/// `stop_file` on Linux.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct Launch {
    pub args: Vec<String>,
    /// Pipe the launched program's stdout and stderr to the caller
    /// ([`crate::Session::take_output`]) instead of inheriting decanter's.
    /// Under Proton, `umu-run`'s: Proton then logs to stderr at the level
    /// `PROTON_LOG` would, rather than to its own file beside the prefix,
    /// so one stream carries the whole run.
    pub capture_output: bool,
    /// `(dll name, mode)` pairs, e.g. `("Galaxy64", "n,b")`.
    ///
    /// Only needed to override a DLL Wine itself provides: a proxy sitting
    /// in the game's own directory already wins, because that is where
    /// Wine looks first for a non-system DLL.
    pub dll_overrides: Vec<(String, String)>,
    /// Extra environment, applied last — so it wins over everything above.
    pub env: Vec<(String, String)>,
    /// Host path to the executable. Under Proton, a bare name (`wineboot`)
    /// is left for Wine to resolve inside the prefix, and
    /// [`Prefix::resolve_windows_path`] gets a path from a program's stored
    /// `C:\...` one; everywhere else it must exist as a file.
    pub executable: PathBuf,
    /// umu's game identity, which is what protonfixes keys its per-game
    /// workarounds off. Convention is `umu-<storefront id>`.
    pub game_id: String,
    /// Runs the launch under GameMode through this `gamemoded`
    /// ([`decanter_core::gamemode_prefix`]).
    pub gamemoded: Option<PathBuf>,
    /// `(game key, title)` the launch's scope is named and described by —
    /// see [`decanter_core::own_scope`]. `None` names it after `game_id`
    /// and the executable, for what isn't a game of its own (an
    /// installer, `wineboot`).
    pub identity: Option<(String, String)>,
    /// Names the session for [`crate::stop`] and [`crate::is_live`]; one
    /// live session per key.
    pub key: String,
    /// A Linux program, run as itself rather than through `umu-run`: no
    /// Proton, no Wine, and the prefix only names the session (see
    /// `Prefix::stop`). Everything Proton-specific here is ignored.
    pub native: bool,
    /// The prefix a launch under Proton runs against, which a Linux
    /// program run natively only takes its session's name from. Created by
    /// umu on first use, so a path that doesn't exist yet is fine — that
    /// is how a new prefix gets made.
    pub prefix: Option<Prefix>,
    pub proton: Proton,
    /// A file the game watches for, whose creation asks it to end on its own, for a game nothing
    /// else asks in a way it heeds: a Job Object can only be killed, and a window's close may not
    /// reach the game. Created at the first request of [`crate::stop`], before the others, and
    /// removed before the spawn and once the session has ended, so a stale one can't end the next
    /// run at its start.
    pub stop_file: Option<PathBuf>,
    /// umu's `STORE`, used alongside `game_id` to look up protonfixes.
    pub store: Option<String>,
    /// Defaults to the executable's own directory. Games that load data
    /// by relative path — most of them — need this, and Proton does not
    /// infer it.
    pub working_directory: Option<PathBuf>,
}

impl Launch {
    #[inline]
    #[must_use]
    pub fn arg<S>(mut self, arg: S) -> Self
    where
        S: Into<String>,
    {
        self.args.push(arg.into());
        self
    }

    #[inline]
    #[must_use]
    pub fn dll_override<D, M>(mut self, dll: D, mode: M) -> Self
    where
        D: Into<String>,
        M: Into<String>,
    {
        self.dll_overrides.push((dll.into(), mode.into()));
        self
    }

    #[inline]
    #[must_use]
    pub fn env<K, V>(mut self, key: K, value: V) -> Self
    where
        K: Into<String>,
        V: Into<String>,
    {
        self.env.push((key.into(), value.into()));
        self
    }

    #[inline]
    #[must_use]
    pub fn game_id<G>(mut self, game_id: G) -> Self
    where
        G: Into<String>,
    {
        self.game_id = game_id.into();
        self
    }

    #[inline]
    pub fn new<P, K>(executable: P, key: K) -> Self
    where
        P: Into<PathBuf>,
        K: Into<String>,
    {
        Self {
            executable: executable.into(),
            args: Vec::new(),
            capture_output: false,
            working_directory: None,
            game_id: String::new(),
            identity: None,
            key: key.into(),
            native: false,
            prefix: None,
            store: None,
            stop_file: None,
            proton: Proton::default(),
            dll_overrides: Vec::new(),
            env: Vec::new(),
            gamemoded: None,
        }
    }

    #[inline]
    #[must_use]
    pub fn prefix(mut self, prefix: Prefix) -> Self {
        self.prefix = Some(prefix);
        self
    }

    #[inline]
    #[must_use]
    pub fn proton(mut self, proton: Proton) -> Self {
        self.proton = proton;
        self
    }

    #[inline]
    #[must_use]
    pub fn store<S>(mut self, store: S) -> Self
    where
        S: Into<String>,
    {
        self.store = Some(store.into());
        self
    }
}
