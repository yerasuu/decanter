//! Gamepads read straight from their evdev nodes, which the desktop session
//! may read without the window's focus: the chord reaches decanter while
//! the game has it. Select + Start rather than anything with Guide, which
//! Steam and psport's in-game close prompt both answer to on their own.

use std::collections::HashSet;
use std::fs::{File, OpenOptions};
use std::io::{self, Read as _};
use std::os::fd::{AsFd as _, AsRawFd, RawFd};
use std::os::unix::fs::OpenOptionsExt as _;
use std::path::{Path, PathBuf};

use futures::StreamExt as _;
use futures::stream::{BoxStream, SelectAll};
use nix::sys::inotify::{AddWatchFlags, InitFlags, Inotify};
use tokio::io::unix::AsyncFd;
use tokio::time::Instant;

use super::{HOLD, until};

const INPUT_DIR: &str = "/dev/input";
const EV_SYN: u16 = 0x00;
const EV_KEY: u16 = 0x01;
const SYN_DROPPED: u16 = 0x03;
const BTN_SELECT: u16 = 0x13a;
const BTN_START: u16 = 0x13b;
/// `struct input_event` on 64-bit Linux: a 16-byte `timeval`, then its
/// type, code and value.
const EVENT_SIZE: usize = 24;

enum Hold {
    Released,
    Since(Instant),
    /// Already asked; held on, it does not ask again until released.
    Asked,
}

pub struct Pads {
    open: HashSet<PathBuf>,
    /// Logged once each instead of every rescan.
    refused: HashSet<PathBuf>,
    /// Each pad's chord state as it changes, `None` once it is gone.
    events: SelectAll<BoxStream<'static, (PathBuf, Option<bool>)>>,
    held: HashSet<PathBuf>,
    hold: Hold,
    /// `None` without inotify: pads plugged in after the game started are
    /// then missed.
    hotplug: Option<AsyncFd<Hotplug>>,
    /// Cleared only once a rescan finishes, so one cut short by a cancelled
    /// [`Pads::chord`] runs again on the next call.
    stale: bool,
}

impl Pads {
    pub fn new() -> Self {
        let hotplug = Hotplug::watch()
            .inspect_err(|err| tracing::warn!(error = %err, "cannot watch for gamepads plugged in during a game"))
            .ok();
        Self {
            open: HashSet::new(),
            refused: HashSet::new(),
            events: SelectAll::new(),
            held: HashSet::new(),
            hold: Hold::Released,
            hotplug,
            stale: true,
        }
    }

    /// Once the chord has been held for [`HOLD`] on any pad, once per hold.
    /// Cancel-safe: what it saw so far stays here.
    pub async fn chord(&mut self) {
        loop {
            if self.stale {
                self.rescan().await;
                self.stale = false;
            }
            let due = match self.hold {
                Hold::Since(since) => Some(since + HOLD),
                Hold::Released | Hold::Asked => None,
            };
            tokio::select! {
                changed = plugged(self.hotplug.as_ref()) => {
                    // udev grants the session access to a new node only after
                    // creating it, so a node refused a moment ago gets a retry.
                    for path in changed {
                        self.refused.remove(&path);
                    }
                    self.stale = true;
                }
                Some((path, held)) = self.events.next() => self.update(path, held),
                () = until(due) => {
                    self.hold = Hold::Asked;
                    return;
                }
            }
        }
    }

    fn update(&mut self, path: PathBuf, held: Option<bool>) {
        match held {
            Some(true) => {
                self.held.insert(path);
            }
            Some(false) => {
                self.held.remove(&path);
            }
            None => {
                self.held.remove(&path);
                self.open.remove(&path);
            }
        }
        self.hold = match (&self.hold, self.held.is_empty()) {
            (_, true) => Hold::Released,
            (Hold::Released, false) => Hold::Since(Instant::now()),
            (Hold::Since(since), false) => Hold::Since(*since),
            (Hold::Asked, false) => Hold::Asked,
        };
    }

    async fn rescan(&mut self) {
        let skip: HashSet<PathBuf> = self.open.union(&self.refused).cloned().collect();
        let found = match tokio::task::spawn_blocking(move || scan(&skip)).await {
            Ok(found) => found,
            Err(err) => {
                tracing::warn!(error = %err, "gamepad scan failed");
                return;
            }
        };
        for (path, opened) in found {
            match opened.and_then(AsyncFd::new) {
                Ok(file) => {
                    tracing::info!(path = %path.display(), "watching gamepad for the close-game chord");
                    self.open.insert(path.clone());
                    self.events.push(events(path, file));
                }
                Err(err) => {
                    tracing::info!(path = %path.display(), error = %err, "cannot read gamepad, its chord is ignored");
                    self.refused.insert(path);
                }
            }
        }
    }
}

/// `/dev/input` watched for nodes created or given new permissions.
struct Hotplug(Inotify);

impl AsRawFd for Hotplug {
    fn as_raw_fd(&self) -> RawFd {
        self.0.as_fd().as_raw_fd()
    }
}

impl Hotplug {
    fn watch() -> io::Result<AsyncFd<Self>> {
        let inotify = crate::achv_try!(Inotify::init(InitFlags::IN_NONBLOCK | InitFlags::IN_CLOEXEC));
        crate::achv_try!(inotify.add_watch(INPUT_DIR, AddWatchFlags::IN_CREATE | AddWatchFlags::IN_ATTRIB));
        AsyncFd::new(Self(inotify))
    }
}

/// The event nodes that changed. Never resolves without inotify, and stops
/// resolving if it fails. Cancel-safe.
async fn plugged(hotplug: Option<&AsyncFd<Hotplug>>) -> Vec<PathBuf> {
    let Some(hotplug) = hotplug else {
        return std::future::pending().await;
    };
    loop {
        let mut guard = match hotplug.readable().await {
            Ok(guard) => guard,
            Err(err) => {
                tracing::warn!(error = %err, "gamepad hot-plug watch failed");
                return std::future::pending().await;
            }
        };
        let Ok(read) = guard.try_io(|fd| fd.get_ref().0.read_events().map_err(io::Error::from)) else {
            continue;
        };
        match read {
            Ok(events) => {
                return events
                    .into_iter()
                    .filter_map(|event| event.name)
                    .filter(|name| name.to_string_lossy().starts_with("event"))
                    .map(|name| Path::new(INPUT_DIR).join(name))
                    .collect();
            }
            Err(err) => {
                tracing::warn!(error = %err, "gamepad hot-plug watch failed");
                return std::future::pending().await;
            }
        }
    }
}

/// Event nodes with both Select and Start, which keyboards and mice lack,
/// opened non-blocking for [`AsyncFd`].
fn scan(skip: &HashSet<PathBuf>) -> Vec<(PathBuf, io::Result<File>)> {
    let Ok(entries) = std::fs::read_dir("/sys/class/input") else {
        return Vec::new();
    };
    entries
        .filter_map(Result::ok)
        .filter(|entry| entry.file_name().to_string_lossy().starts_with("event"))
        .filter(|entry| {
            std::fs::read_to_string(entry.path().join("device/capabilities/key"))
                .is_ok_and(|bitmap| has_key(&bitmap, BTN_SELECT) && has_key(&bitmap, BTN_START))
        })
        .map(|entry| Path::new(INPUT_DIR).join(entry.file_name()))
        .filter(|path| !skip.contains(path))
        .map(|path| {
            let opened = OpenOptions::new()
                .read(true)
                .custom_flags(nix::fcntl::OFlag::O_NONBLOCK.bits())
                .open(&path);
            (path, opened)
        })
        .collect()
}

/// sysfs prints a capability bitmap as hex `unsigned long`s (64 bits here),
/// the highest first.
fn has_key(bitmap: &str, code: u16) -> bool {
    let code = usize::from(code);
    bitmap
        .split_whitespace()
        .rev()
        .nth(code / 64)
        .and_then(|word| u64::from_str_radix(word, 16).ok())
        .is_some_and(|word| word & (1 << (code % 64)) != 0)
}

fn events(path: PathBuf, file: AsyncFd<File>) -> BoxStream<'static, (PathBuf, Option<bool>)> {
    let pad = Pad {
        file,
        select: false,
        start: false,
    };
    futures::stream::unfold(Some(pad), move |pad| {
        let path = path.clone();
        async move {
            let Some(mut pad) = pad else {
                return None;
            };
            match pad.next_change().await {
                Ok(held) => Some(((path, Some(held)), Some(pad))),
                Err(err) => {
                    tracing::info!(path = %path.display(), error = %err, "gamepad gone");
                    Some(((path, None), None))
                }
            }
        }
    })
    .boxed()
}

struct Pad {
    file: AsyncFd<File>,
    select: bool,
    start: bool,
}

impl Pad {
    const fn held(&self) -> bool {
        self.select && self.start
    }

    /// Reads until the chord is pressed or let go. An error means the pad
    /// is gone (`ENODEV` once unplugged).
    async fn next_change(&mut self) -> io::Result<bool> {
        let mut buffer = [0_u8; EVENT_SIZE * 32];
        loop {
            let mut guard = match self.file.readable().await {
                Ok(guard) => guard,
                Err(err) => return Err(err),
            };
            let Ok(read) = guard.try_io(|file| {
                let mut file = file.get_ref();
                file.read(&mut buffer)
            }) else {
                continue;
            };
            let read = match read {
                Ok(0) => return Err(io::ErrorKind::UnexpectedEof.into()),
                Ok(read) => read,
                Err(err) => return Err(err),
            };
            let before = self.held();
            for event in buffer.get(..read).unwrap_or_default().as_chunks::<EVENT_SIZE>().0 {
                self.apply(event);
            }
            if self.held() != before {
                return Ok(self.held());
            }
        }
    }

    const fn apply(&mut self, event: &[u8; EVENT_SIZE]) {
        let kind = u16::from_ne_bytes([event[16], event[17]]);
        let code = u16::from_ne_bytes([event[18], event[19]]);
        // A value of 2 is autorepeat: still down.
        let down = i32::from_ne_bytes([event[20], event[21], event[22], event[23]]) != 0;
        match (kind, code) {
            (EV_KEY, BTN_SELECT) => self.select = down,
            (EV_KEY, BTN_START) => self.start = down,
            // The kernel dropped events: what is down is unknown until pressed again.
            (EV_SYN, SYN_DROPPED) => {
                self.select = false;
                self.start = false;
            }
            _ => {}
        }
    }
}
