//! The names that reach the filesystem: paths inside a mod or a game, mod ids, account and game ids.

use core::fmt;
use std::path::{Path, PathBuf};

use crate::{Error, Result};

const MAX_PATH_BYTES: usize = 1024;
const MAX_COMPONENTS: usize = 64;

/// A relative, `/`-separated path that can only land inside the folder it is joined to.
///
/// No empty, `.` or `..` component, no root, and none of `\`, `:` or control characters, since on
/// a Windows host `\` separates and `C:x` names another drive.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct ModPath(String);

impl ModPath {
    /// # Errors
    ///
    /// [`Error::InvalidPath`] when `text` breaks any of the rules above or is longer than 1024
    /// bytes or 64 components.
    #[inline]
    pub fn parse(text: &str) -> Result<Self> {
        let invalid = || Error::InvalidPath(text.to_owned());
        if text.is_empty() || text.len() > MAX_PATH_BYTES {
            return Err(invalid());
        }
        let mut count = 0_usize;
        for component in text.split('/') {
            count = count.saturating_add(1);
            if component.is_empty()
                || component == "."
                || component == ".."
                || component.chars().any(|ch| ch.is_control() || ch == '\\' || ch == ':')
            {
                return Err(invalid());
            }
        }
        if count > MAX_COMPONENTS {
            return Err(invalid());
        }
        Ok(Self(text.to_owned()))
    }

    /// A path read from a folder on disk, one name at a time.
    pub(crate) fn from_components<'name>(names: impl IntoIterator<Item = &'name str>) -> Result<Self> {
        let mut text = String::new();
        for (index, name) in names.into_iter().enumerate() {
            if index > 0 {
                text.push('/');
            }
            text.push_str(name);
        }
        Self::parse(&text)
    }

    #[must_use]
    #[inline]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The key two paths are the same file by for a Windows game, which Wine opens
    /// case-insensitively. `to_lowercase` is close enough to Unicode simple case folding for the
    /// names mods carry (design, section 2).
    #[must_use]
    #[inline]
    pub fn folded(&self) -> String {
        self.0.to_lowercase()
    }

    #[inline]
    pub fn components(&self) -> impl Iterator<Item = &str> {
        self.0.split('/')
    }

    #[must_use]
    #[inline]
    pub fn file_name(&self) -> &str {
        self.0.rsplit('/').next().unwrap_or(&self.0)
    }

    #[must_use]
    #[inline]
    pub fn depth(&self) -> usize {
        self.components().count()
    }

    #[must_use]
    #[inline]
    pub fn under(&self, base: &Path) -> PathBuf {
        self.components().fold(base.to_path_buf(), |path, name| path.join(name))
    }

    /// `self` inside `folder`, `folder` being a mod's `target` (`None` for the game's root).
    ///
    /// # Errors
    ///
    /// [`Error::InvalidPath`] when both together are longer than a path may be.
    #[inline]
    pub fn inside(&self, folder: Option<&Self>) -> Result<Self> {
        folder.map_or_else(
            || Ok(self.clone()),
            |base| Self::parse(&format!("{}/{}", base.0, self.0)),
        )
    }
}

impl fmt::Display for ModPath {
    #[inline]
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A mod's id: a uuid in its hyphenated lowercase form, which is also its folder's name.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ModId(String);

impl ModId {
    pub(crate) fn new() -> Self {
        Self(uuid::Uuid::new_v4().hyphenated().to_string())
    }

    /// # Errors
    ///
    /// [`Error::InvalidId`] for anything but a uuid in hyphenated lowercase form.
    #[inline]
    pub fn parse(text: &str) -> Result<Self> {
        match uuid::Uuid::try_parse(text) {
            Ok(id) if id.hyphenated().to_string() == text => Ok(Self(text.to_owned())),
            Ok(_) | Err(_) => Err(Error::InvalidId(text.to_owned())),
        }
    }

    #[must_use]
    #[inline]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ModId {
    #[inline]
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// An account or game id as one folder or file name: the rule manifests already follow (ASCII
/// alphanumerics, `_`, `-`).
pub(crate) fn id_component(text: &str) -> Result<&str> {
    if text.is_empty()
        || text.len() > 128
        || !text
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '-')
    {
        return Err(Error::InvalidName(text.to_owned()));
    }
    Ok(text)
}
