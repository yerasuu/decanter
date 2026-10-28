//! The dropped file, opened read-only, with every read bounded: by its
//! length, by a cap per read and by a budget for the whole probe, so a
//! header that points at gigabytes costs nothing.

use std::fs::{File, OpenOptions};
use std::io::{self, Read as _, Seek as _, SeekFrom};
use std::path::Path;

/// The most one read takes: a resource entry, an image, a block of headers.
pub(super) const MAX_READ: usize = 2 * 1024 * 1024;
/// The most a whole probe reads from one file.
const BUDGET: u64 = 8 * 1024 * 1024;

pub(super) struct Source {
    file: File,
    len: u64,
    budget: u64,
}

impl Source {
    /// Opens `path` without blocking on it, and only if what was opened is
    /// a regular file: a FIFO swapped in after the caller looked would
    /// otherwise hang the open or the first read.
    pub(super) fn open(path: &Path) -> io::Result<Self> {
        let mut options = OpenOptions::new();
        options.read(true);
        #[cfg(target_os = "linux")]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options.custom_flags(nix::fcntl::OFlag::O_NONBLOCK.bits());
        }
        let file = options.open(path)?;
        let metadata = file.metadata()?;
        if !metadata.is_file() {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "not a regular file"));
        }
        Ok(Self {
            file,
            len: metadata.len(),
            budget: BUDGET,
        })
    }

    pub(super) const fn len(&self) -> u64 {
        self.len
    }

    /// Exactly `len` bytes at `offset`, or `None` when they run past the
    /// end of the file, past [`MAX_READ`] or past what is left of the budget.
    pub(super) fn read(&mut self, offset: u64, len: usize) -> Option<Vec<u8>> {
        let wanted = u64::try_from(len).ok()?;
        if len > MAX_READ || wanted > self.budget || offset.checked_add(wanted)? > self.len {
            return None;
        }
        self.budget = self.budget.saturating_sub(wanted);
        self.file.seek(SeekFrom::Start(offset)).ok()?;
        let mut bytes = Vec::with_capacity(len);
        (&mut self.file).take(wanted).read_to_end(&mut bytes).ok()?;
        (bytes.len() == len).then_some(bytes)
    }

    /// At most `len` bytes at `offset`, fewer at the end of the file.
    pub(super) fn read_up_to(&mut self, offset: u64, len: usize) -> Option<Vec<u8>> {
        let available = usize::try_from(self.len.checked_sub(offset)?).unwrap_or(usize::MAX);
        self.read(offset, len.min(available))
    }
}

pub(super) fn u16_le(bytes: &[u8], at: usize) -> Option<u16> {
    bytes
        .get(at..at.checked_add(2)?)?
        .try_into()
        .ok()
        .map(u16::from_le_bytes)
}

pub(super) fn u32_le(bytes: &[u8], at: usize) -> Option<u32> {
    bytes
        .get(at..at.checked_add(4)?)?
        .try_into()
        .ok()
        .map(u32::from_le_bytes)
}
