//! A UTF-8 byte order mark in front of a file the user edits by hand. Windows `PowerShell` 5.1's
//! `Set-Content -Encoding utf8` writes one, and `serde_json` and `toml` refuse it. Every reader of
//! such a file goes through here, so decanter's own writes stay BOM-free.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};

const BOM: &[u8] = b"\xEF\xBB\xBF";

static TOLD: LazyLock<Mutex<HashSet<PathBuf>>> = LazyLock::new(Mutex::default);

fn note(path: &Path) {
    if let Ok(mut told) = TOLD.lock()
        && told.insert(path.to_path_buf())
    {
        tracing::debug!(path = %path.display(), "leading UTF-8 BOM ignored");
    }
}

/// `data` without a leading BOM; the first time a path has one it is said at debug.
#[must_use]
pub fn strip<'data>(data: &'data [u8], path: &Path) -> &'data [u8] {
    let Some(rest) = data.strip_prefix(BOM) else {
        return data;
    };
    note(path);
    rest
}

/// [`strip`] for text.
#[must_use]
pub fn strip_str<'data>(text: &'data str, path: &Path) -> &'data str {
    let Some(rest) = text.strip_prefix('\u{feff}') else {
        return text;
    };
    note(path);
    rest
}
