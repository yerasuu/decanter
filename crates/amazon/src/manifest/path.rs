// SPDX-License-Identifier: GPL-3.0-or-later
//! The one place a manifest's path is checked. nile joins them under the install directory as they
//! come (docs/amazon-nile-integration-analysis.md, section 4.2), so a signed manifest naming
//! `..\..\.bashrc` would write outside it; here such a path refuses the manifest instead.

use core::fmt;
use std::path::Path;

/// Longer than Windows' classic `MAX_PATH` (260), which the launcher's games are built against.
const MAX_PATH_LEN: usize = 1024;
/// `NAME_MAX` of the file systems a library lives on.
const MAX_COMPONENT_LEN: usize = 255;

/// A path relative to the install directory: `/`-separated, with no empty, `.` or `..` component,
/// no root, drive or colon and no control character, so joining it under a directory stays there.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ManifestPath(String);

/// Why a path was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum PathFault {
    Empty,
    TooLong,
    ComponentTooLong,
    ControlCharacter,
    /// A drive (`C:`) or an NTFS stream (`file:stream`); Linux would make it a plain name, which the
    /// Windows game could never open.
    Colon,
    Absolute,
    EmptyComponent,
    DotComponent,
}

impl fmt::Display for PathFault {
    #[inline]
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match *self {
            Self::Empty => "it is empty",
            Self::TooLong => "it is longer than 1024 bytes",
            Self::ComponentTooLong => "a component is longer than 255 bytes",
            Self::ControlCharacter => "it holds a control character",
            Self::Colon => "it names a drive or a stream",
            Self::Absolute => "it is absolute",
            Self::EmptyComponent => "it has an empty component",
            Self::DotComponent => "it has a `.` or `..` component",
        })
    }
}

impl ManifestPath {
    /// The manifest separates with `\` and sometimes `/`; both become `/`.
    pub(crate) fn parse(raw: &str) -> Result<Self, PathFault> {
        if raw.is_empty() {
            return Err(PathFault::Empty);
        }
        if raw.len() > MAX_PATH_LEN {
            return Err(PathFault::TooLong);
        }
        if raw.chars().any(char::is_control) {
            return Err(PathFault::ControlCharacter);
        }
        if raw.contains(':') {
            return Err(PathFault::Colon);
        }
        let path = raw.replace('\\', "/");
        if path.starts_with('/') {
            return Err(PathFault::Absolute);
        }
        for component in path.split('/') {
            if component.is_empty() {
                return Err(PathFault::EmptyComponent);
            }
            if component == "." || component == ".." {
                return Err(PathFault::DotComponent);
            }
            if component.len() > MAX_COMPONENT_LEN {
                return Err(PathFault::ComponentTooLong);
            }
        }
        Ok(Self(path))
    }

    #[inline]
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The path to join under the install directory; `/` separates on Windows too.
    #[inline]
    #[must_use]
    pub fn as_path(&self) -> &Path {
        Path::new(&self.0)
    }

    /// The path folded for comparisons that ignore case, since Windows games open files by any case.
    pub(super) fn folded(&self) -> String {
        self.0.to_lowercase()
    }
}

/// The directories a folded path sits in, deepest last: `a/b/c` gives `a` and `a/b`. Lowercasing
/// maps `/` to itself, so these are also the folded forms of the original's ancestors.
pub(super) fn ancestors(folded: &str) -> impl Iterator<Item = &str> {
    folded.match_indices('/').filter_map(|(end, _)| folded.get(..end))
}
