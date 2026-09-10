//! Resolving what to download: build selection, depot manifest, and a
//! signed CDN URL for every chunk — everything [`super::install_into`]
//! needs before a single byte moves.

use anyhow::Context;
use decanter_runtime::sanitize_relative_path;

use super::download::ChunkSource;

/// A file to fetch and where it goes, resolved from the depot manifest.
pub(super) struct PlannedFile {
    pub(super) relative_path: String,
    pub(super) chunks: Vec<ChunkSource>,
}

pub(super) struct Plan {
    pub(super) version: String,
    pub(super) install_directory: String,
    pub(super) install_size_bytes: Option<u64>,
    pub(super) files: Vec<PlannedFile>,
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
/// the DLC's product id.
pub(super) async fn plan(
    access_token: &str,
    build_owner: i64,
    content: i64,
    branch: Option<&str>,
    build_id: Option<&str>,
) -> anyhow::Result<Plan> {
    let http = crate::http_client();
    let (install_directory, version, depot_files) =
        crate::achv_try!(resolve_depot_files(&http, access_token, build_owner, content, branch, build_id).await);
    // Signed for the content's own product: its chunks live under that
    // product's path, and a link signed for the build's owner is refused.
    let secure_link = crate::achv_try!(
        crate::depot::get_secure_link(&http, access_token, content, "/")
            .await
            .inspect_err(|err| tracing::error!(content, error = %err, "gog secure_link lookup failed, aborting plan"))
            .context("GOG secure_link lookup failed")
    );

    let mut files = Vec::with_capacity(depot_files.len());
    let mut install_size_bytes = Some(0u64);

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
        let mut file_size = Some(0u64);
        for chunk in depot_file.chunks {
            // Skipping a chunk would write a file that's silently short —
            // an install that "succeeded" but produced a corrupt game. Fail
            // the whole thing instead.
            let Some(url) = secure_link
                .iter()
                .find_map(|entry| crate::depot::build_chunk_url(entry, &chunk.compressed_md5))
            else {
                tracing::error!(relative_path, "no usable gog cdn endpoint for a chunk, aborting plan");
                anyhow::bail!("no usable GOG CDN endpoint for a chunk of {relative_path}");
            };
            let url =
                crate::achv_try!(url::Url::parse(&url).with_context(|| format!("bad chunk URL for {relative_path}")));
            file_size = file_size.map(|total| total + chunk.size);
            chunks.push(ChunkSource { url, compressed: true });
        }

        install_size_bytes = install_size_bytes.zip(file_size).map(|(a, b)| a + b);
        files.push(PlannedFile { relative_path, chunks });
    }

    tracing::debug!(
        content,
        file_count = files.len(),
        install_size_bytes,
        "resolved gog install plan"
    );
    Ok(Plan {
        version,
        install_directory,
        install_size_bytes,
        files,
    })
}

/// The install directory, build id and every file of `content`'s depots.
///
/// Split out because removing a DLC needs the file list and nothing else:
/// no secure link, no chunk URLs, no download.
pub(super) async fn resolve_depot_files(
    http: &reqwest::Client,
    access_token: &str,
    build_owner: i64,
    content: i64,
    branch: Option<&str>,
    build_id: Option<&str>,
) -> anyhow::Result<(String, String, Vec<crate::depot::DepotFile>)> {
    let builds = crate::achv_try!(
        crate::depot::get_builds(http, access_token, build_owner)
            .await
            .inspect_err(|err| tracing::error!(build_owner, error = %err, "gog builds lookup failed, aborting plan"))
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
                None => format!("no builds available for GOG game {build_owner}"),
            }
        )),
    };

    let meta = crate::achv_try!(
        crate::depot::get_build_meta(http, access_token, &build.link)
            .await
            .inspect_err(|err| tracing::error!(build_id = build.build_id, error = %err, "gog build manifest fetch failed, aborting plan"))
            .context("GOG build manifest fetch failed")
    );
    let depots = crate::depot::select_depots(&meta, content);
    if depots.is_empty() {
        tracing::error!(
            content,
            build_id = build.build_id,
            "no windows-compatible depot, aborting plan"
        );
        anyhow::bail!("no Windows-compatible depot for GOG product {content}");
    }

    let mut depot_files = Vec::new();
    for depot in depots {
        depot_files.extend(crate::achv_try!(
            crate::depot::get_depot_files(http, &depot.manifest)
                .await
                .inspect_err(|err| tracing::error!(manifest = depot.manifest, error = %err, "gog depot manifest fetch failed, aborting plan"))
                .context("GOG depot manifest fetch failed")
        ));
    }
    Ok((meta.install_directory, build.build_id.clone(), depot_files))
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
) -> anyhow::Result<Vec<std::path::PathBuf>> {
    let http = crate::http_client();
    let (_, _, dlc_files) = crate::achv_try!(resolve_depot_files(&http, access_token, parent, dlc, branch, None).await);
    let (_, _, base_files) =
        crate::achv_try!(resolve_depot_files(&http, access_token, parent, parent, branch, None).await);

    let shared: std::collections::HashSet<String> = base_files.into_iter().map(|file| file.path).collect();
    Ok(dlc_files
        .into_iter()
        .filter(|file| !shared.contains(&file.path))
        .filter_map(|file| manifest_relative_path(&file.path))
        .collect())
}
