//! A cheap stand-in for "did this install change since it was measured":
//! measuring a game walks every file, while this only stats its top two
//! levels of folders and a few named files, so a size measured once can be
//! trusted until the fingerprint moves.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

/// FNV-1a, since the value is persisted and must not change between builds
/// the way `std`'s hasher may.
struct Fnv(u64);

impl Fnv {
    /// By name, not full path, so a game moved whole to another folder
    /// keeps its fingerprint and isn't measured again.
    fn stamp(&mut self, path: &Path) {
        self.write(path.file_name().unwrap_or_default().as_encoded_bytes());
        if let Ok(meta) = fs::metadata(path) {
            let modified = meta
                .modified()
                .ok()
                .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
                .map_or(0, |since| since.as_nanos());
            self.write(&modified.to_le_bytes());
            if meta.is_file() {
                self.write(&meta.len().to_le_bytes());
            }
        }
    }

    /// The folders directly under `dir`, in name order since `read_dir`'s
    /// order isn't stable.
    fn subdirs(dir: &Path) -> Vec<PathBuf> {
        let mut dirs: Vec<_> = fs::read_dir(dir)
            .into_iter()
            .flatten()
            .flatten()
            .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
            .map(|entry| entry.path())
            .collect();
        dirs.sort();
        dirs
    }

    fn write(&mut self, bytes: &[u8]) {
        for byte in bytes {
            self.0 = (self.0 ^ u64::from(*byte)).wrapping_mul(0x0100_0000_01b3);
        }
    }
}

/// Changes when a folder is added, removed or renamed within the top two
/// levels of any of `dirs`, or when one of `files` is rewritten. The roots'
/// own times are left out: a Wine prefix rewrites its registry beside
/// `drive_c` on every run without anything installed changing.
#[must_use]
#[inline]
pub fn install_fingerprint(dirs: &[&Path], files: &[&Path]) -> String {
    let mut hash = Fnv(0xcbf2_9ce4_8422_2325);
    for dir in dirs {
        for child in Fnv::subdirs(dir) {
            hash.stamp(&child);
            for grandchild in Fnv::subdirs(&child) {
                hash.stamp(&grandchild);
            }
        }
    }
    for file in files {
        hash.stamp(file);
    }
    format!("{:016x}", hash.0)
}
