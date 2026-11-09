// SPDX-License-Identifier: GPL-3.0-or-later
//! What a mod's `mod.toml` records of where it came from on Nexus. decanter-mods' `[origin]` has
//! one `package` field for the platform's id, which for Nexus is three parts: it is written
//! `<domain>-<mod>-<file>` and read from the right, since the two numbers never hold a `-`.
//! `version` is the file's version cut down to the characters an origin takes, for display; the
//! update check compares file numbers, not versions, since authors type versions freely.

use decanter_mods::ModOrigin;

use crate::files::NexusFile;
use crate::id::{FileId, GameDomain, NexusModId};
use crate::{Error, PLATFORM, Result};

const MAX_VERSION: usize = 64;

#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct NexusOrigin {
    pub domain: GameDomain,
    pub mod_id: NexusModId,
    pub file_id: FileId,
    pub version: String,
}

impl NexusOrigin {
    /// The Nexus origin of `origin`, `None` when it is another platform's.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidReference`] when a Nexus origin's package does not read back.
    #[inline]
    pub fn read(origin: &ModOrigin) -> Result<Option<Self>> {
        if origin.platform != PLATFORM {
            return Ok(None);
        }
        let invalid = || Error::InvalidReference(origin.package.clone());
        let mut parts = origin.package.rsplitn(3, '-');
        let (Some(file), Some(number), Some(domain)) = (parts.next(), parts.next(), parts.next()) else {
            return Err(invalid());
        };
        Ok(Some(Self {
            domain: domain.parse::<GameDomain>().map_err(|_err| invalid())?,
            mod_id: number.parse().map_err(|_err| invalid())?,
            file_id: file.parse().map_err(|_err| invalid())?,
            version: origin.version.clone(),
        }))
    }
}

/// The origin of `file` of mod `id`, downloaded from `host`.
///
/// # Errors
///
/// When a field can't be an origin's, which checked names never make.
#[inline]
pub fn origin(domain: &GameDomain, id: NexusModId, file: &NexusFile, host: &str) -> Result<ModOrigin> {
    let cleaned: String = file
        .version
        .as_deref()
        .unwrap_or_default()
        .chars()
        .filter_map(|character| match character {
            'a'..='z' | 'A'..='Z' | '0'..='9' | '.' | '-' | '_' => Some(character),
            ' ' => Some('_'),
            _ => None,
        })
        .take(MAX_VERSION)
        .collect();
    let version = if cleaned.is_empty() {
        format!("file-{}", file.id)
    } else {
        cleaned
    };
    Ok(ModOrigin::new(
        PLATFORM,
        &format!("{domain}-{id}-{}", file.id),
        &version,
        if host.is_empty() { "nexusmods.com" } else { host },
    )?)
}
