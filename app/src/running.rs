//! Which games are being played, whichever decanter process started them: the window's Play, or
//! the CLI a Steam shortcut or a task bar entry runs. Each session holds an exclusive lock on a
//! file of its own beside the database (`running/<key>.lock`) for as long as it lasts, with who
//! holds it in `<key>.json`, apart since a Windows lock also bars reading the locked file. The OS
//! drops a lock with its process ([`File::try_lock`]), so a holder that crashed never reads as
//! running, and a second start of the same game finds the lock taken and refuses.
//!
//! The lock files are left in place once released: removing one would let a process that opened
//! it just before lock the removed file while a third one creates and locks a new one.
//!
//! [`File::try_lock`]: https://doc.rust-lang.org/std/fs/struct.File.html#method.try_lock

use std::collections::HashSet;
use std::fs::{File, OpenOptions, TryLockError};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use gpui::{AsyncApp, WeakEntity};
use serde::{Deserialize, Serialize};

use crate::state::Background;

/// How often the window looks for games another decanter process started or ended.
const TICK: Duration = Duration::from_secs(2);

/// The game a session is of, as each section finds it again.
#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Game {
    Pc {
        account_id: String,
        game_id: String,
    },
    /// Claimed by title, since two dumps of one title share its saves.
    Ps {
        title_id: String,
        dir: PathBuf,
    },
}

impl Game {
    pub fn pc(account_id: &str, game_id: &str) -> Self {
        Self::Pc {
            account_id: account_id.to_owned(),
            game_id: game_id.to_owned(),
        }
    }

    pub fn ps(title_id: &str, dir: &Path) -> Self {
        Self::Ps {
            title_id: title_id.to_owned(),
            dir: dir.to_owned(),
        }
    }

    fn key(&self) -> String {
        match self {
            Self::Pc { account_id, game_id } => format!(
                "pc.{}.{}",
                decanter_core::game_key(account_id),
                decanter_core::game_key(game_id)
            ),
            Self::Ps { title_id, .. } => format!("ps.{}", decanter_core::game_key(title_id)),
        }
    }
}

#[derive(Serialize, Deserialize)]
struct Holder {
    pid: u32,
    /// Seconds since the Unix epoch.
    started: u64,
    game: Game,
}

/// A game's session, held until dropped.
#[must_use = "the game reads as not running as soon as this is dropped"]
pub struct Claim {
    _lock: File,
    holder: PathBuf,
}

impl Drop for Claim {
    fn drop(&mut self) {
        if let Err(err) = std::fs::remove_file(&self.holder)
            && err.kind() != std::io::ErrorKind::NotFound
        {
            tracing::warn!(path = %self.holder.display(), %err, "could not remove the running game's record");
        }
    }
}

/// The game is already being played, by this process or another one.
#[derive(Debug, thiserror::Error)]
#[error("the game is already running (decanter process {})", pid.map_or_else(|| "unknown".to_owned(), |pid| pid.to_string()))]
pub struct Busy {
    pub pid: Option<u32>,
}

fn dir() -> Option<PathBuf> {
    decanter_core::bootstrap::active_db_path()?
        .parent()
        .map(|data| data.join("running"))
}

fn paths(dir: &Path, key: &str) -> (PathBuf, PathBuf) {
    (dir.join(format!("{key}.lock")), dir.join(format!("{key}.json")))
}

/// Claims `game`'s session for this process. `Ok(None)` when it can't be recorded (no library, a
/// folder that can't be written): the game still starts, only other processes won't see it.
///
/// # Errors
/// [`Busy`] while any process, this one included, holds the game's session.
pub fn claim(game: Game) -> Result<Option<Claim>, Busy> {
    let Some(dir) = dir() else {
        tracing::warn!("no library to record the running game in, other decanter processes won't see it");
        return Ok(None);
    };
    if let Err(err) = std::fs::create_dir_all(&dir) {
        tracing::warn!(dir = %dir.display(), %err, "could not create the running games' folder, other decanter processes won't see the game");
        return Ok(None);
    }
    let key = game.key();
    let (lock_path, holder_path) = paths(&dir, &key);
    let file = match OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&lock_path)
    {
        Ok(file) => file,
        Err(err) => {
            tracing::warn!(path = %lock_path.display(), %err, "could not open the running game's lock, other decanter processes won't see the game");
            return Ok(None);
        }
    };
    match file.try_lock() {
        Ok(()) => {}
        Err(TryLockError::WouldBlock) => {
            let pid = read_holder(&holder_path).map(|holder| holder.pid);
            tracing::info!(key, pid, "the game is already running, not starting it again");
            return Err(Busy { pid });
        }
        Err(TryLockError::Error(err)) => {
            tracing::warn!(path = %lock_path.display(), %err, "could not lock the running game's record, other decanter processes won't see the game");
            return Ok(None);
        }
    }
    let holder = Holder {
        pid: std::process::id(),
        started: SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map_or(0, |since| since.as_secs()),
        game,
    };
    let written = serde_json::to_vec(&holder)
        .map_err(std::io::Error::other)
        .and_then(|bytes| std::fs::write(&holder_path, bytes));
    if let Err(err) = written {
        tracing::warn!(path = %holder_path.display(), %err, "could not record who runs the game, other decanter processes see it running but can't stop it");
    }
    tracing::debug!(key, pid = holder.pid, "game session claimed");
    Ok(Some(Claim {
        _lock: file,
        holder: holder_path,
    }))
}

fn read_holder(path: &Path) -> Option<Holder> {
    let bytes = std::fs::read(path).ok()?;
    serde_json::from_slice(&bytes)
        .inspect_err(|err| tracing::warn!(path = %path.display(), %err, "unreadable running game record"))
        .ok()
}

/// Who holds the session at `lock_path` now, `None` once nobody does.
fn live_holder(lock_path: &Path, holder_path: &Path) -> Option<Holder> {
    let file = File::open(lock_path).ok()?;
    match file.try_lock_shared() {
        Ok(()) => None,
        Err(TryLockError::WouldBlock) => read_holder(holder_path),
        Err(TryLockError::Error(err)) => {
            tracing::debug!(path = %lock_path.display(), %err, "could not probe a running game's lock");
            None
        }
    }
}

/// The games other decanter processes are playing now. Blocking: a folder read and a lock probe
/// per game ever played.
fn others() -> Vec<Game> {
    let Some(dir) = dir() else {
        return Vec::new();
    };
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let own = std::process::id();
    entries
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let lock_path = entry.path();
            let key = lock_path.file_name()?.to_str()?.strip_suffix(".lock")?.to_owned();
            let (_, holder_path) = paths(&dir, &key);
            live_holder(&lock_path, &holder_path)
        })
        .filter(|holder| holder.pid != own)
        .map(|holder| holder.game)
        .collect()
}

/// Asks the decanter process playing `game` to stop it, as Steam's Stop asks it: that process
/// stops the game and keeps its books (playtime, cloud saves, the run's log and failures). The
/// holder is read again here, so a process that ended since the window last looked is never
/// signalled under a reused pid.
pub fn stop(game: &Game) {
    let Some(dir) = dir() else {
        return;
    };
    let key = game.key();
    let (lock_path, holder_path) = paths(&dir, &key);
    let Some(holder) = live_holder(&lock_path, &holder_path) else {
        tracing::info!(key, "the game another decanter process played has ended already");
        return;
    };
    if holder.pid == std::process::id() {
        return;
    }
    signal_stop(&key, holder.pid);
}

#[cfg(target_os = "linux")]
fn signal_stop(key: &str, pid: u32) {
    use nix::sys::signal::{Signal, kill};
    use nix::unistd::Pid;
    // 0 and 1 would signal a process group or init (POSIX `kill`), never a decanter process.
    let Some(raw) = i32::try_from(pid).ok().filter(|raw| *raw > 1) else {
        tracing::warn!(key, pid, "unusable pid in the running game's record, not signalled");
        return;
    };
    match kill(Pid::from_raw(raw), Signal::SIGTERM) {
        Ok(()) => tracing::info!(key, pid, "asked the decanter process playing the game to stop it"),
        Err(err) => tracing::warn!(key, pid, %err, "could not ask the decanter process playing the game to stop it"),
    }
}

#[cfg(not(target_os = "linux"))]
fn signal_stop(key: &str, pid: u32) {
    tracing::warn!(
        key,
        pid,
        "stopping a game another decanter process plays is not supported on this system"
    );
}

/// Shows in the window what other decanter processes are playing, as running and stoppable, and
/// takes it off once they end.
pub async fn watch(background: WeakEntity<Background>, cx: &mut AsyncApp) {
    loop {
        cx.background_executor().timer(TICK).await;
        let games = cx.background_executor().spawn(async { others() }).await;
        let mut pc = HashSet::new();
        let mut ps = HashSet::new();
        for game in games {
            match game {
                Game::Pc { game_id, .. } => {
                    pc.insert(game_id);
                }
                Game::Ps { dir, .. } => {
                    ps.insert(dir);
                }
            }
        }
        if background
            .update(cx, |background, cx| background.set_elsewhere(pc, cx))
            .is_err()
        {
            return;
        }
        cx.update(|cx| crate::state::Ps::set_elsewhere(ps, cx));
    }
}
