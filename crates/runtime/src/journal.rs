//! What a download needs to survive a crash, a `kill -9` or a power cut.
//!
//! A lock keeps two decanter processes out of one staging folder, files are replaced whole or not
//! at all, and an append-only log tells its torn last line apart from a good one by a CRC.
//!
//! GOG's chunked installs keep their progress with these; the resumable single-file fetch is meant
//! to reuse them, which is why nothing here knows what a record means.

use std::io;
use std::path::{Path, PathBuf};

use tokio::io::AsyncWriteExt as _;

const LOCK_FILE: &str = "lock";

/// Holds a staging folder for this process until dropped.
#[must_use = "the folder is unlocked as soon as this is dropped"]
pub struct StagingLock {
    _file: std::fs::File,
}

#[derive(Debug, thiserror::Error)]
pub enum LockError {
    #[error("another decanter process is using {}", .0.display())]
    Busy(PathBuf),
    #[error("couldn't lock {}: {source}", path.display())]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

/// Locks `dir` (which must exist) through a `lock` file inside it. The OS drops the lock with the
/// process, so a crashed owner never leaves the folder locked.
///
/// # Errors
/// [`LockError::Busy`] while another handle holds it, in this process or another one.
#[inline]
pub fn lock(dir: &Path) -> Result<StagingLock, LockError> {
    let path = dir.join(LOCK_FILE);
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&path)
        .map_err(|source| LockError::Io {
            path: path.clone(),
            source,
        })?;
    match file.try_lock() {
        Ok(()) => Ok(StagingLock { _file: file }),
        Err(std::fs::TryLockError::WouldBlock) => Err(LockError::Busy(dir.to_path_buf())),
        Err(std::fs::TryLockError::Error(source)) => Err(LockError::Io { path, source }),
    }
}

/// Makes a rename or a new entry in `dir` durable. Windows has no handle to sync a folder through,
/// and NTFS journals its metadata, so there it does nothing.
///
/// # Errors
/// When `dir` can't be opened or synced.
#[inline]
pub async fn sync_dir(dir: &Path) -> io::Result<()> {
    #[cfg(unix)]
    tokio::fs::File::open(dir).await?.sync_all().await?;
    #[cfg(not(unix))]
    let _ = dir;
    Ok(())
}

/// Replaces `path` with `bytes` so that a crash leaves the old file or the new one, never a mix:
/// written aside, synced, renamed over it, then the rename synced.
///
/// # Errors
/// When any step fails; the file aside is removed then.
#[inline]
pub async fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let Some(dir) = path.parent() else {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "no parent folder"));
    };
    let mut aside = path.as_os_str().to_owned();
    aside.push(".tmp");
    let aside = PathBuf::from(aside);
    let written = async {
        let mut file = tokio::fs::File::create(&aside).await?;
        file.write_all(bytes).await?;
        file.sync_all().await?;
        tokio::fs::rename(&aside, path).await
    }
    .await;
    if let Err(err) = written {
        if let Err(cleanup) = tokio::fs::remove_file(&aside).await
            && cleanup.kind() != io::ErrorKind::NotFound
        {
            tracing::warn!(path = %aside.display(), error = %cleanup, "couldn't remove a half-written file");
        }
        return Err(err);
    }
    sync_dir(dir).await
}

/// An append-only log of one-line records, each followed by a tab and the CRC-32 of the record in
/// hex. A line is durable once [`LineLog::append`] returns.
pub struct LineLog {
    file: tokio::fs::File,
}

impl LineLog {
    /// Starts `path` over with `first` as its only record.
    ///
    /// # Errors
    /// When the file can't be written or synced.
    #[inline]
    pub async fn create(path: &Path, first: &[u8]) -> io::Result<Self> {
        let mut log = Self {
            file: tokio::fs::File::create(path).await?,
        };
        log.append(first).await?;
        if let Some(dir) = path.parent() {
            sync_dir(dir).await?;
        }
        Ok(log)
    }

    /// Opens `path` to append to it and returns its good records, in order. Reading stops at the
    /// first line whose CRC doesn't match (a write the crash tore), and the file is cut back to
    /// the end of the last good line, so the next record isn't glued onto the torn one.
    ///
    /// # Errors
    /// When the file can't be read, cut or opened.
    #[inline]
    pub async fn open(path: &Path) -> io::Result<(Self, Vec<Vec<u8>>)> {
        let contents = tokio::fs::read(path).await?;
        let (records, good_len) = parse(&contents);
        let file = tokio::fs::OpenOptions::new().write(true).open(path).await?;
        if good_len < contents.len() {
            tracing::warn!(
                path = %path.display(),
                dropped_bytes = contents.len() - good_len,
                "dropping a torn tail from a download log"
            );
            file.set_len(u64::try_from(good_len).unwrap_or(u64::MAX)).await?;
            file.sync_data().await?;
        }
        let mut file = file;
        tokio::io::AsyncSeekExt::seek(&mut file, io::SeekFrom::End(0)).await?;
        Ok((Self { file }, records))
    }

    /// Appends `record` and syncs it.
    ///
    /// # Errors
    /// When the write or the sync fails; the record may then be torn, which [`LineLog::open`]
    /// drops.
    ///
    /// `record` must not contain a newline: lines are what records are told apart by.
    #[inline]
    pub async fn append(&mut self, record: &[u8]) -> io::Result<()> {
        if record.contains(&b'\n') {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "a log record can't hold a newline",
            ));
        }
        let mut line = Vec::with_capacity(record.len() + 10);
        line.extend_from_slice(record);
        line.extend_from_slice(format!("\t{:08x}\n", crc32(record)).as_bytes());
        self.file.write_all(&line).await?;
        self.file.flush().await?;
        self.file.sync_data().await
    }
}

/// The good records of the log at `path`, read without touching it, for a reader that doesn't own
/// the folder.
///
/// # Errors
/// When the file can't be read.
#[inline]
pub async fn read_records(path: &Path) -> io::Result<Vec<Vec<u8>>> {
    Ok(parse(&tokio::fs::read(path).await?).0)
}

/// The good records and how many bytes they span.
fn parse(contents: &[u8]) -> (Vec<Vec<u8>>, usize) {
    let mut records = Vec::new();
    let mut good_len = 0;
    for line in contents.split_inclusive(|&byte| byte == b'\n') {
        let Some(body) = line.strip_suffix(b"\n") else {
            break;
        };
        let Some(tab) = body.iter().rposition(|&byte| byte == b'\t') else {
            break;
        };
        let (record, crc) = body.split_at(tab);
        let matches = std::str::from_utf8(crc.get(1..).unwrap_or_default())
            .ok()
            .and_then(|hex| u32::from_str_radix(hex, 16).ok())
            .is_some_and(|crc| crc == crc32(record));
        if !matches {
            break;
        }
        records.push(record.to_vec());
        good_len += line.len();
    }
    (records, good_len)
}

/// CRC-32 (IEEE), bit by bit: log lines are tens of bytes, so a table buys nothing, and the
/// workspace has no CRC crate of its own to depend on.
fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = u32::MAX;
    for &byte in bytes {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
        }
    }
    !crc
}
