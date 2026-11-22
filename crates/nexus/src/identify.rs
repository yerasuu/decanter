//! Finding which Nexus file an archive the user added by hand is. Only the archive's name survives
//! the import, and Nexus names its downloads `<mod name>-<mod id>-<version>-<upload time>.<ext>`,
//! so the mod id comes out of the name and the file is the one of that mod's list that has it.

use crate::files::{FileList, NexusFile, files};
use crate::http::Client;
use crate::id::{GameDomain, NexusModId};
use crate::listing::{ModInfo, mod_info};
use crate::{Error, Result};

/// The mod and file a name points at.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct Identified {
    pub info: ModInfo,
    pub file: NexusFile,
    /// How many files of the mod are still offered, so a name can tell them apart.
    pub current_files: usize,
}

impl Identified {
    /// The name the mod should carry: the mod's, with the file's when the mod offers several.
    #[must_use]
    #[inline]
    pub fn display_name(&self) -> String {
        if self.current_files > 1 && !self.file.name.is_empty() && self.file.name != self.info.name {
            format!("{} - {}", self.info.name, self.file.name)
        } else {
            self.info.name.clone()
        }
    }
}

const MAX_CANDIDATES: usize = 4;

/// The mod ids and upload time a Nexus download name could carry; empty for any other name.
///
/// The version is dashed too (`1-0-2`), so every all-digit part between the name and
/// the upload time is a candidate, and only the mod's file list says which one is right.
#[must_use]
#[inline]
pub fn parse_file_name(source: &str) -> (Vec<NexusModId>, i64) {
    let without_extension = source.rsplit_once('.').map_or(source, |(stem, _extension)| stem);
    let stem = without_extension
        .strip_suffix(')')
        .and_then(|rest| rest.rsplit_once(" ("))
        .map_or(without_extension, |(rest, _copy)| rest);
    let pieces: Vec<&str> = stem.split('-').collect();
    let Some((last, before)) = pieces.split_last() else {
        return (Vec::new(), 0);
    };
    let Some(uploaded) = last.parse::<i64>().ok().filter(|time| *time > 0) else {
        return (Vec::new(), 0);
    };
    let candidates = before
        .iter()
        .enumerate()
        .skip(1)
        .filter_map(|(_, piece)| piece.parse::<u64>().ok().and_then(NexusModId::new))
        .take(MAX_CANDIDATES)
        .collect();
    (candidates, uploaded)
}

fn normalised(name: &str) -> String {
    name.trim().replace(' ', "_").to_lowercase()
}

fn pick<'list>(list: &'list FileList, source: &str, uploaded: i64) -> Option<&'list NexusFile> {
    let wanted = normalised(source);
    list.files
        .iter()
        .find(|file| file.file_name.as_deref().is_some_and(|name| normalised(name) == wanted))
        .or_else(|| list.files.iter().find(|file| file.uploaded == Some(uploaded)))
}

/// The Nexus mod and file the archive named `source` is, `None` when its name is not a Nexus
/// one or the mod's list has no such file.
///
/// # Errors
///
/// As a request.
#[inline]
pub async fn identify(client: &Client, domain: &GameDomain, source: &str) -> Result<Option<Identified>> {
    let (candidates, uploaded) = parse_file_name(source);
    let mut found = None;
    for mod_id in candidates {
        let list = match files(client, domain, mod_id).await {
            Ok(list) => list,
            Err(Error::NotFound { .. }) => continue,
            Err(err) => return Err(err),
        };
        if let Some(file) = pick(&list, source, uploaded).cloned() {
            let current_files = list.files.iter().filter(|listed| listed.category.is_current()).count();
            found = Some((mod_id, file, current_files));
            break;
        }
    }
    let Some((mod_id, file, current_files)) = found else {
        return Ok(None);
    };
    let info = mod_info(client, domain, mod_id).await?;
    Ok(Some(Identified {
        info,
        file,
        current_files,
    }))
}
