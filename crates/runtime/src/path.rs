//! Joining paths from the network or from a game's files onto a directory
//! decanter owns, without letting them climb out of it.

use std::path::{Component, Path, PathBuf};

/// Reduces a caller-supplied relative path to something that can only ever
/// land inside the directory it is joined to: no absolute paths, no `..`,
/// no drive prefixes.
///
/// Every path decanter joins onto a prefix or a data directory comes from
/// a storefront's manifest or a product id, i.e. from the network.
#[inline]
#[must_use]
pub fn sanitize_relative_path(path: &str) -> Option<PathBuf> {
    let source = Path::new(path);
    let mut sanitized = PathBuf::new();
    for component in source.components() {
        match component {
            Component::Normal(part) => sanitized.push(part),
            // `.` is meaningless rather than dangerous; the rest are how a
            // relative path escapes upward.
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                tracing::warn!(path, "rejecting path: climbs out of or escapes its base directory");
                return None;
            }
        }
    }
    if sanitized.as_os_str().is_empty() {
        tracing::warn!(path, "rejecting path: sanitizes to nothing");
        return None;
    }
    Some(sanitized)
}
