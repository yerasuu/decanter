//! Resolving what to download: build selection, depot manifest, and a
//! signed CDN URL for every chunk — everything [`super::install_into`]
//! needs before a single byte moves.

use anyhow::Context;
use decanter_core::GameSystem;
use decanter_runtime::sanitize_relative_path;

use super::download::{ChunkSource, PlannedDownload};
use crate::api::client::{DepotFile, Sessionless};
use crate::language::Resolved;

/// A file to fetch and where it goes, resolved from the depot manifest.
pub(super) struct PlannedFile {
    pub(super) relative_path: String,
    pub(super) chunks: Vec<ChunkSource>,
    /// Needs the executable mode once it lands: only a macOS build's files
    /// say so, since a Windows build runs by extension, under Proton or
    /// natively.
    pub(super) is_executable: bool,
}

/// GOG's depot chunks inflate to about a megabyte (as `gogdl` reads them; GOG
/// doesn't document it). The bound is far above that so no real chunk is
/// refused, and only stops a broken manifest from sizing buffers.
pub(super) const MAX_CHUNK_SIZE: usize = 64 * 1024 * 1024;

pub(super) struct Plan {
    pub(super) system: GameSystem,
    pub(super) version: String,
    pub(super) install_directory: String,
    pub(super) files: Vec<PlannedFile>,
    /// The language whose depots were taken; `None` when every depot was.
    pub(super) language: Option<Resolved>,
}

/// Which of a product's depots a plan takes, by language.
#[derive(Clone, Copy)]
pub(super) struct Languages<'wanted> {
    /// Best first; `None` takes every language the build has.
    pub(super) wanted: Option<&'wanted [String]>,
    /// Only the depots of the language picked, for a language added to a game already installed:
    /// what every language needs is on disk already.
    pub(super) is_only_language: bool,
}

impl Languages<'_> {
    pub(super) const ALL: Self = Self {
        wanted: None,
        is_only_language: false,
    };
}

/// What planning only one language's depots fails with when the build has no depot of it for
/// that product.
#[derive(Debug, thiserror::Error)]
#[error("this build doesn't offer the language asked for")]
pub(super) struct LanguageNotOffered;

/// What [`resolve_depot_files`] read of one product in one build.
pub(super) struct DepotFiles {
    pub(super) install_directory: String,
    pub(super) build_id: String,
    pub(super) files: Vec<DepotFile>,
    pub(super) language: Option<Resolved>,
}

impl Plan {
    pub(super) fn downloads(&self) -> Vec<PlannedDownload> {
        self.files
            .iter()
            .map(|file| PlannedDownload {
                relative_path: file.relative_path.clone(),
                chunks: file.chunks.clone(),
                is_executable: file.is_executable,
                owner: None,
            })
            .collect()
    }
}

/// Depot manifests address files with Windows separators
/// (`Game_Data\file.assets`). On Linux `\` is an ordinary filename
/// character, so sanitizing one of those directly yields a single
/// component with a literal backslash in its name instead of a
/// subdirectory — the game then can't find its own data folder. Convert
/// first, then sanitize, so `..` and friends are still rejected.
pub(super) fn manifest_relative_path(path: &str) -> Option<std::path::PathBuf> {
    sanitize_relative_path(&path.replace('\\', "/"))
}

/// Resolves what to download: which build, which depots, every file in
/// them, and a signed CDN URL per chunk.
///
/// `build_owner` is whose build to read and `content` is whose depots to
/// take out of it. They differ for a DLC: a DLC has no build of its own —
/// its content ships inside the base game's build, as depots tagged with
/// the DLC's product id. `system` is whose build that is: a game's Windows
/// and macOS builds are separate, with depots of their own.
pub(super) async fn plan(
    access_token: &str,
    build_owner: i64,
    content: i64,
    branch: Option<&str>,
    build_id: Option<&str>,
    system: GameSystem,
    languages: Languages<'_>,
) -> anyhow::Result<Plan> {
    let http = crate::achv_try!(Sessionless::new().context("building the GOG HTTP client"));
    let DepotFiles {
        install_directory,
        build_id: version,
        files: depot_files,
        language,
    } = crate::achv_try!(
        resolve_depot_files(
            &http,
            access_token,
            build_owner,
            content,
            branch,
            build_id,
            system,
            languages
        )
        .await
    );
    // Signed for the content's own product: its chunks live under that
    // product's path, and a link signed for the build's owner is refused.
    let secure_link = crate::achv_try!(
        http.secure_link(access_token, content, "/")
            .await
            .inspect_err(|err| tracing::error!(content, error = %err, "gog secure_link lookup failed, aborting plan"))
            .context("GOG secure_link lookup failed")
    );

    let mut files = Vec::with_capacity(depot_files.len());

    for depot_file in depot_files {
        // Manifest paths come from a remote server and land on our disk —
        // reject anything that escapes the install root.
        let relative_path = crate::achv_try!(
            manifest_relative_path(&depot_file.path)
                .with_context(|| format!("unsafe path in GOG manifest: {}", depot_file.path))
        )
        .to_string_lossy()
        .into_owned();

        let mut chunks = Vec::with_capacity(depot_file.chunks.len());
        for chunk in depot_file.chunks {
            // Skipping a chunk would write a file that's silently short —
            // an install that "succeeded" but produced a corrupt game. Fail
            // the whole thing instead.
            let Some(url) = secure_link
                .iter()
                .find_map(|entry| entry.chunk_url(&chunk.compressed_md5))
            else {
                tracing::error!(relative_path, "no usable gog cdn endpoint for a chunk, aborting plan");
                anyhow::bail!("no usable GOG CDN endpoint for a chunk of {relative_path}");
            };
            let url =
                crate::achv_try!(url::Url::parse(&url).with_context(|| format!("bad chunk URL for {relative_path}")));
            // Checked here, where the manifest comes in, so the download
            // compares fixed bytes and sizes buffers it can trust.
            let (Some(compressed_md5), Some(md5)) = (parse_md5(&chunk.compressed_md5), parse_md5(&chunk.md5)) else {
                anyhow::bail!("malformed chunk checksum in the GOG manifest for {relative_path}");
            };
            let chunk_size = crate::achv_try!(
                usize::try_from(chunk.size)
                    .ok()
                    .filter(|&chunk_size| chunk_size <= MAX_CHUNK_SIZE)
                    .with_context(|| format!("implausible chunk size {} for {relative_path}", chunk.size))
            );
            chunks.push(ChunkSource {
                url,
                compressed_md5,
                md5,
                size: chunk_size,
            });
        }

        let is_executable = system == GameSystem::MacOs && depot_file.flags.iter().any(|flag| flag == "executable");
        files.push(PlannedFile {
            relative_path,
            chunks,
            is_executable,
        });
    }

    tracing::debug!(content, file_count = files.len(), language = ?language, "resolved gog install plan");
    Ok(Plan {
        system,
        version,
        install_directory,
        files,
        language,
    })
}

/// A manifest's hex MD5, as bytes; `None` for anything but 32 hex digits.
pub(super) fn parse_md5(hex: &str) -> Option<[u8; 16]> {
    let (pairs, rest) = hex.as_bytes().as_chunks::<2>();
    if pairs.len() != 16 || !rest.is_empty() {
        return None;
    }
    let mut digest = [0u8; 16];
    for (byte, &[high, low]) in digest.iter_mut().zip(pairs) {
        let (Some(high), Some(low)) = (char::from(high).to_digit(16), char::from(low).to_digit(16)) else {
            return None;
        };
        let Ok(value) = u8::try_from(high * 16 + low) else {
            return None;
        };
        *byte = value;
    }
    Some(digest)
}

/// The install directory, build id and every file of `content`'s depots, of the language
/// `languages` picks.
///
/// Split out because removing a DLC needs the file list and nothing else:
/// no secure link, no chunk URLs, no download.
#[expect(
    clippy::too_many_arguments,
    reason = "each names a different part of which build and depots to read"
)]
pub(super) async fn resolve_depot_files(
    http: &Sessionless,
    access_token: &str,
    build_owner: i64,
    content: i64,
    branch: Option<&str>,
    build_id: Option<&str>,
    system: GameSystem,
    languages: Languages<'_>,
) -> anyhow::Result<DepotFiles> {
    let builds = crate::achv_try!(
        crate::depot::get_builds(http, access_token, build_owner, system)
            .await
            .inspect_err(
                |err| tracing::error!(build_owner, %system, error = %err, "gog builds lookup failed, aborting plan")
            )
            .context("GOG builds lookup failed")
    );
    // An explicit build id is a manual pin -- a rollback to a specific
    // historical build rather than "whichever build this branch currently
    // points at". Falls back to the branch lookup when there isn't one.
    let build = match build_id {
        Some(id) => crate::achv_try!(
            crate::depot::select_build_by_id(&builds, id)
                .with_context(|| format!("build {id} is not offered for GOG game {build_owner}"))
        ),
        None => crate::achv_try!(crate::depot::select_build_for_branch(&builds, branch).with_context(
            || match branch {
                Some(name) => format!("branch {name:?} is not offered for GOG game {build_owner}"),
                None => format!("no {system} builds available for GOG game {build_owner}"),
            }
        )),
    };

    let meta = crate::achv_try!(
        http.build_meta(access_token, &build.link)
            .await
            .inspect_err(|err| tracing::error!(build_id = build.build_id, error = %err, "gog build manifest fetch failed, aborting plan"))
            .context("GOG build manifest fetch failed")
    );
    let crate::depot::DepotSelection { mut depots, language } =
        crate::depot::select_depots(&meta, content, system, languages.wanted);
    if languages.is_only_language {
        let Some(picked) = language.as_ref().filter(|picked| picked.is_wanted) else {
            tracing::error!(content, build_id = build.build_id, wanted = ?languages.wanted, "the build doesn't offer the language asked for");
            return Err(LanguageNotOffered.into());
        };
        depots.retain(|depot| crate::depot::specific(depot).is_some());
        tracing::info!(
            content,
            language = picked.tag,
            depot_count = depots.len(),
            "planning only the depots of one language"
        );
    }
    // A DLC can be an entitlement only (a vehicle or cosmetic the game's own files already
    // carry): its build lists no depot of its own, and installing it records it with no files.
    let is_entitlement = build_owner != content && crate::depot::product_depots(&meta, content, system).is_empty();
    if depots.is_empty() && !is_entitlement {
        tracing::error!(
            content,
            %system,
            build_id = build.build_id,
            "no compatible depot, aborting plan"
        );
        anyhow::bail!("no {system}-compatible depot for GOG product {content}");
    }
    if is_entitlement {
        tracing::info!(
            content,
            build_id = build.build_id,
            "the dlc has no depot of its own, planning no files"
        );
    }

    let mut depot_files = Vec::new();
    for depot in depots {
        depot_files.extend(crate::achv_try!(
            http.depot_files(&depot.manifest)
                .await
                .inspect_err(|err| tracing::error!(manifest = depot.manifest, error = %err, "gog depot manifest fetch failed, aborting plan"))
                .context("GOG depot manifest fetch failed")
        ));
    }
    // Two depots can ship the same path (each language depot of a DLC carries its own
    // goggame-<id>.info and .hashdb). The files of a run are written under their path, so the
    // copies would race on one staging file; the later depot overrides the earlier, as in
    // an install of every depot in order.
    let mut seen = std::collections::HashSet::new();
    let mut unique: Vec<_> = depot_files
        .into_iter()
        .rev()
        .filter(|file| seen.insert(file.path.clone()))
        .collect();
    unique.reverse();
    Ok(DepotFiles {
        install_directory: meta.install_directory,
        build_id: build.build_id.clone(),
        files: unique,
        language,
    })
}

/// The files a DLC installed that the base game does not also ship,
/// relative to the install directory.
///
/// The subtraction is the point. A DLC's files land mixed into the base
/// game's directory, and the two depots can name the same path — a patched
/// executable, a shared asset bundle. Deleting those would break the game
/// the DLC was removed from, so only what is unique to the DLC goes.
pub(super) async fn dlc_only_files(
    access_token: &str,
    parent: i64,
    dlc: i64,
    branch: Option<&str>,
    system: GameSystem,
) -> anyhow::Result<Vec<std::path::PathBuf>> {
    let http = crate::achv_try!(Sessionless::new().context("building the GOG HTTP client"));
    let dlc_files = crate::achv_try!(
        resolve_depot_files(&http, access_token, parent, dlc, branch, None, system, Languages::ALL).await
    );
    let base_files = crate::achv_try!(
        resolve_depot_files(
            &http,
            access_token,
            parent,
            parent,
            branch,
            None,
            system,
            Languages::ALL
        )
        .await
    );

    let shared: std::collections::HashSet<String> = base_files.files.into_iter().map(|file| file.path).collect();
    Ok(dlc_files
        .files
        .into_iter()
        .filter(|file| !shared.contains(&file.path))
        .filter_map(|file| manifest_relative_path(&file.path))
        .collect())
}
