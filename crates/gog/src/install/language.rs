// SPDX-License-Identifier: GPL-3.0-or-later
//! The language a GOG game is installed in, after the fact: adding another language's depots to
//! a game on disk, finding the files of the languages it no longer uses, and writing the language
//! into its `goggame-<id>.info`.
//!
//! Adding a language is a download like any other (journal, queue, pause, resume), planned only
//! from the depots of that language at the build the game is on. Removing files is never done on
//! its own: [`unused_language_files`] lists them for the user to confirm (#1133), and
//! [`remove_unused_languages`] deletes only what that list named and still is a regular file.

use std::collections::{BTreeSet, HashSet};
use std::path::Path;

use anyhow::Context as _;
use decanter_core::{InstalledGame, Library, Storefront, platform};

use super::download::PlannedDownload;
use super::journal::{Header, Kind, Request};
use super::pending::Pending;
use super::plan::{LanguageNotOffered, Languages, manifest_relative_path};
use super::staging::staging_root;
use super::tracking::{InstallKey, active, claim};
use super::{Progress, Settling};
use crate::api::client::Sessionless;
use crate::hook::REAL_DLL_BACKUP;

/// Far above any real `goggame-<id>.info`, which runs to a few kilobytes.
const INFO_MAX_BYTES: u64 = 1024 * 1024;

/// Every language `product_id`'s depots are split by in the build `build_id` pins, or the newest
/// of `branch` (`None` is mainline), as GOG tags in GOG's order. Empty when the build doesn't split
/// its files by language, which leaves nothing to choose.
///
/// # Errors
/// When GOG can't be asked, or doesn't offer that build or branch.
pub async fn offered_languages(
    account_id: &str,
    product_id: &str,
    branch: Option<&str>,
    build_id: Option<&str>,
) -> anyhow::Result<Vec<String>> {
    let game_id: i64 = crate::achv_try!(product_id.parse().context("GOG product_id must be numeric"));
    let access_token = crate::achv_try!(super::live_token(account_id).await);
    let http = crate::achv_try!(Sessionless::new().context("building the GOG HTTP client"));
    let meta = crate::achv_try!(build_meta(&http, access_token.expose(), game_id, branch, build_id).await);
    let offered = crate::depot::offered_languages(&meta, game_id, platform::game_system());
    tracing::debug!(product_id, ?offered, "read the languages a gog build offers");
    Ok(offered)
}

async fn build_meta(
    http: &Sessionless,
    access_token: &str,
    game_id: i64,
    branch: Option<&str>,
    build_id: Option<&str>,
) -> anyhow::Result<crate::api::client::BuildMeta> {
    let builds = crate::achv_try!(
        crate::depot::get_builds(http, access_token, game_id, platform::game_system())
            .await
            .context("GOG builds lookup failed")
    );
    let build = match build_id {
        Some(id) => crate::achv_try!(
            crate::depot::select_build_by_id(&builds, id)
                .with_context(|| format!("build {id} is not offered for GOG game {game_id}"))
        ),
        None => crate::achv_try!(
            crate::depot::select_build_for_branch(&builds, branch)
                .with_context(|| format!("no build of that branch is offered for GOG game {game_id}"))
        ),
    };
    http.build_meta(access_token, &build.link)
        .await
        .context("GOG build manifest fetch failed")
}

/// Downloads the files of `language` (a GOG tag the game offers) into an installed game and its
/// installed DLC, at the build the game is on, and records it as the game's language.
///
/// Only that language's own depots are fetched: what every language needs is on disk already.
/// The files of the language it had stay until [`remove_unused_languages`] is confirmed. A DLC
/// that doesn't offer `language` is left as it is.
///
/// # Errors
/// As [`super::install`]; also when the game isn't installed or its build doesn't offer
/// `language`.
pub async fn add_language(
    library: &Library,
    account_id: &str,
    product_id: &str,
    language: &str,
    on_progress: impl FnMut(Progress) + Send + 'static,
) -> anyhow::Result<()> {
    let game = crate::achv_try!(
        library
            .read_manifest(account_id, Storefront::Gog, product_id)
            .with_context(|| format!("{product_id} is not installed"))
    );
    let request = Request {
        kind: Kind::Install,
        account_id: account_id.to_owned(),
        product_id: product_id.to_owned(),
        parent_id: None,
        title: game.name.clone(),
        branch: game.branch.clone(),
        build_id: Some(game.build_id.clone()),
        languages: Some(vec![crate::language::canonical(language)]),
        is_only_language: true,
    };
    add_language_request(library, request, on_progress).await
}

/// [`add_language`] of a request as it was journalled, for a resume. It waits in the same queue
/// as every download and gives its place up, or keeps it, the same way.
pub(super) async fn add_language_request(
    library: &Library,
    request: Request,
    on_progress: impl FnMut(Progress) + Send + 'static,
) -> anyhow::Result<()> {
    let key: InstallKey = (request.account_id.clone(), request.product_id.clone());
    let outcome = add_language_run(library, request, on_progress).await;
    super::queue::settle(&key, &outcome).await;
    outcome
}

async fn add_language_run(
    library: &Library,
    request: Request,
    mut on_progress: impl FnMut(Progress) + Send + 'static,
) -> anyhow::Result<()> {
    let account_id = request.account_id.clone();
    let product_id = request.product_id.clone();
    let (account_id, product_id) = (account_id.as_str(), product_id.as_str());
    let Some(tag) = request.languages.as_ref().and_then(|wanted| wanted.first()).cloned() else {
        anyhow::bail!("a language change of {product_id} names no language");
    };
    let game_id: i64 = crate::achv_try!(product_id.parse().context("GOG product_id must be numeric"));
    tracing::info!(product_id, language = tag, "starting a gog language change");

    claim(account_id, product_id).await;
    let key: InstallKey = (account_id.to_owned(), product_id.to_owned());
    let _settling = Settling::start();
    let game = crate::achv_try!(
        library
            .read_manifest(account_id, Storefront::Gog, product_id)
            .with_context(|| format!("{product_id} is not installed"))
    );
    let install_root = crate::achv_try!(
        library
            .install_dir(&game.install_dir)
            .context("unusable install directory")
    );
    let staging = crate::achv_try!(staging_root(&install_root, account_id, product_id));
    let pending = Pending::new(request.clone(), Vec::new());
    let entered = super::pending::enter(&staging, &pending).await;
    let Some(turn) = super::tracking::wait_turn(library, &key).await else {
        return Err(super::stopped_queued(entered, pending, &key).await);
    };
    let build_id = request.build_id.clone().unwrap_or_else(|| game.build_id.clone());
    let planning = async {
        let access_token = crate::achv_try!(super::live_token(account_id).await);
        plan_language(&game, access_token.expose(), game_id, &build_id, &tag).await
    };
    let Some(planned) = super::tracking::until_stopped(&key, planning).await else {
        drop(turn);
        return Err(super::stopped_queued(entered, pending, &key).await);
    };
    let (downloads, products) = match planned {
        Ok(planned) => planned,
        Err(err) => return Err(super::unplanned(entered, err).await),
    };
    let _turn = turn;
    let header = crate::achv_try!(Header::new(
        request,
        build_id,
        game.install_dir.clone(),
        Some(tag.clone()),
        &downloads
    ));
    let replaced = on_disk(&install_root, &header).await;
    let (journal, header) = crate::achv_try!(
        super::fetch_into_staging(&staging, header, downloads, key, entered.lock, &mut on_progress).await
    );
    crate::achv_try!(super::move_staged(&journal, &install_root, product_id).await);

    // Read again: a DLC may have been recorded on it while this downloaded.
    let mut game = library
        .read_manifest(account_id, Storefront::Gog, product_id)
        .unwrap_or(game);
    game.language = Some(tag.clone());
    let added = header.total_bytes.saturating_sub(replaced);
    game.size_on_disk = Some(game.size_on_disk.unwrap_or(0).saturating_add(added));
    crate::achv_try!(
        library
            .write_manifest(&game)
            .inspect_err(|err| tracing::error!(product_id, error = %err, "recording the game's new language failed"))
            .context("recording the game's language")
    );
    for product in &products {
        write_info(&install_root, product, &tag, &[]).await;
    }
    super::finish(journal, product_id).await;
    tracing::info!(
        product_id,
        language = tag,
        bytes = header.total_bytes,
        "language change completed"
    );
    Ok(())
}

/// The bytes the files of `header` already take in `install_root`, which the move replaces.
async fn on_disk(install_root: &Path, header: &Header) -> u64 {
    let mut bytes = 0u64;
    for file in &header.files {
        if let Ok(meta) = tokio::fs::symlink_metadata(install_root.join(&file.path)).await
            && meta.is_file()
        {
            bytes = bytes.saturating_add(meta.len());
        }
    }
    bytes
}

/// The files of `tag`'s depots of the game and of each installed DLC that offers it, at
/// `build_id`, and the products they came from. The game first and its DLC in the manifest's order, so the
/// same request plans the same journal.
async fn plan_language(
    game: &InstalledGame,
    access_token: &str,
    game_id: i64,
    build_id: &str,
    tag: &str,
) -> anyhow::Result<(Vec<PlannedDownload>, Vec<String>)> {
    let wanted = vec![tag.to_owned()];
    let languages = Languages {
        wanted: Some(&wanted),
        is_only_language: true,
    };
    let system = platform::game_system();
    let bundle_program = super::program::bundle_program(game, system);
    let mut downloads = Vec::new();
    let mut products = Vec::new();
    let mut contents = vec![game_id];
    for dlc in game.dlcs.keys() {
        contents.push(crate::achv_try!(
            dlc.parse::<i64>().context("GOG DLC id must be numeric")
        ));
    }
    for content in contents {
        let planned = super::plan::plan(
            access_token,
            game_id,
            content,
            game.branch.as_deref(),
            Some(build_id),
            system,
            languages,
        )
        .await;
        let plan = match planned {
            Ok(plan) => plan,
            Err(err) if content != game_id && err.is::<LanguageNotOffered>() => {
                tracing::info!(
                    content,
                    language = tag,
                    "the dlc doesn't offer the language, left as it is"
                );
                continue;
            }
            Err(err) => return Err(err),
        };
        let owner = content.to_string();
        downloads.extend(plan.files.into_iter().map(|file| PlannedDownload {
            is_executable: file.is_executable || bundle_program == Some(file.relative_path.as_str()),
            relative_path: file.relative_path,
            chunks: file.chunks,
            owner: Some(owner.clone()),
        }));
        products.push(owner);
    }
    Ok((downloads, products))
}

/// One file [`unused_language_files`] found.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct UnusedFile {
    /// Relative to the install directory, as the depot manifest names it.
    pub relative_path: String,
    pub size: u64,
}

/// What removing the languages a game doesn't use would delete: the dry run the user confirms.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct LanguageCleanup {
    /// The build the files were read from; removal is refused once the game is on another.
    pub build_id: String,
    /// The language kept, as a GOG tag; `None` when the build doesn't split its files by language.
    pub language: Option<String>,
    /// False when the game doesn't offer the language asked for and `language` is its fallback.
    pub is_wanted: bool,
    pub files: Vec<UnusedFile>,
    pub total_bytes: u64,
    /// Files of the kept language that aren't on disk. Removal is refused while there are any,
    /// since it would leave the game with no language complete.
    pub missing: usize,
}

/// The files of `account_id`'s installed `product_id` (and its installed DLC) that only depots of
/// other languages than the one `keep` picks list, and that are on disk, with their sizes.
/// Nothing is deleted.
///
/// Read from the depot manifests of the build the game is on, so it needs GOG. A file any kept
/// depot also lists stays, as does anything no depot of the build lists: saves, settings and
/// mods the game or the user put there are never candidates.
///
/// # Errors
/// When the game isn't installed, or GOG can't be asked or no longer offers its build.
pub async fn unused_language_files(
    library: &Library,
    account_id: &str,
    product_id: &str,
    keep: &[String],
) -> anyhow::Result<LanguageCleanup> {
    let game = crate::achv_try!(
        library
            .read_manifest(account_id, Storefront::Gog, product_id)
            .with_context(|| format!("{product_id} is not installed"))
    );
    let install_root = crate::achv_try!(
        library
            .install_dir(&game.install_dir)
            .context("unusable install directory")
    );
    let game_id: i64 = crate::achv_try!(product_id.parse().context("GOG product_id must be numeric"));
    let access_token = crate::achv_try!(super::live_token(account_id).await);
    let http = crate::achv_try!(Sessionless::new().context("building the GOG HTTP client"));
    let system = platform::game_system();
    let meta = crate::achv_try!(
        build_meta(
            &http,
            access_token.expose(),
            game_id,
            game.branch.as_deref(),
            Some(&game.build_id)
        )
        .await
    );

    let mut contents = vec![game_id];
    for dlc in game.dlcs.keys() {
        contents.push(crate::achv_try!(
            dlc.parse::<i64>().context("GOG DLC id must be numeric")
        ));
    }
    let mut picked = None;
    let mut kept_paths = HashSet::new();
    let mut kept_specific = BTreeSet::new();
    let mut other_paths = BTreeSet::new();
    for content in contents {
        let selection = crate::depot::select_depots(&meta, content, system, Some(keep));
        let kept_manifests: HashSet<&str> = selection.depots.iter().map(|depot| depot.manifest.as_str()).collect();
        if content == game_id {
            picked.clone_from(&selection.language);
        }
        for depot in crate::depot::product_depots(&meta, content, system) {
            let is_kept = kept_manifests.contains(depot.manifest.as_str());
            let is_specific = crate::depot::specific(depot).is_some();
            if !is_kept && !is_specific {
                continue;
            }
            let files = crate::achv_try!(
                http.depot_files(&depot.manifest)
                    .await
                    .context("GOG depot manifest fetch failed")
            );
            for file in files {
                let Some(relative) = manifest_relative_path(&file.path) else {
                    continue;
                };
                let relative = relative.to_string_lossy().into_owned();
                if is_kept {
                    kept_paths.insert(relative.to_lowercase());
                    if is_specific {
                        kept_specific.insert(relative);
                    }
                } else {
                    other_paths.insert(relative);
                }
            }
        }
    }

    let mut missing = 0usize;
    for relative in &kept_specific {
        if !tokio::fs::try_exists(install_root.join(relative))
            .await
            .unwrap_or(false)
        {
            missing += 1;
        }
    }
    let mut files = Vec::new();
    let mut seen = HashSet::new();
    for relative in other_paths {
        let lower = relative.to_lowercase();
        if kept_paths.contains(&lower) || is_kept_regardless(&relative) || !seen.insert(lower) {
            continue;
        }
        match tokio::fs::symlink_metadata(install_root.join(&relative)).await {
            Ok(meta) if meta.is_file() => files.push(UnusedFile {
                relative_path: relative,
                size: meta.len(),
            }),
            Ok(_) => {}
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => tracing::warn!(relative, error = %err, "couldn't read an unused language file, leaving it"),
        }
    }
    let total_bytes = files.iter().map(|file| file.size).sum();
    let cleanup = LanguageCleanup {
        build_id: game.build_id.clone(),
        is_wanted: picked.as_ref().is_none_or(|picked| picked.is_wanted),
        language: picked.map(|picked| picked.tag),
        files,
        total_bytes,
        missing,
    };
    tracing::info!(
        product_id,
        language = cleanup.language,
        file_count = cleanup.files.len(),
        bytes = cleanup.total_bytes,
        missing = cleanup.missing,
        "listed the files of unused languages"
    );
    Ok(cleanup)
}

/// Files a depot of another language may list that the game needs whatever its language: the
/// `goggame-<id>.info` the launch and the language are read from, and the Galaxy library the
/// achievement hook stands in for.
fn is_kept_regardless(relative: &str) -> bool {
    let name = Path::new(relative)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    (name.starts_with("goggame-") && name.ends_with(".info"))
        || name == "galaxy64.dll"
        || name == "galaxy.dll"
        || name == REAL_DLL_BACKUP.to_ascii_lowercase()
}

/// Deletes the files `cleanup` listed (the user has confirmed it, #1133) and records its language
/// as the game's. Returns the bytes freed.
///
/// Each path is checked again before it goes: a relative path inside the install directory that
/// is still a regular file, never a directory or a link.
///
/// # Errors
/// When the game isn't installed, is on another build than `cleanup` was read from, is
/// downloading, or misses files of the kept language; or its manifest can't be written.
pub async fn remove_unused_languages(
    library: &Library,
    account_id: &str,
    product_id: &str,
    cleanup: &LanguageCleanup,
) -> anyhow::Result<u64> {
    let mut game = crate::achv_try!(
        library
            .read_manifest(account_id, Storefront::Gog, product_id)
            .with_context(|| format!("{product_id} is not installed"))
    );
    anyhow::ensure!(
        game.build_id == cleanup.build_id,
        "the game changed build since its unused languages were listed; list them again"
    );
    anyhow::ensure!(
        cleanup.missing == 0,
        "{} files of the kept language aren't installed",
        cleanup.missing
    );
    let Some(tag) = cleanup.language.clone() else {
        anyhow::bail!("this build doesn't split its files by language");
    };
    let key: InstallKey = (account_id.to_owned(), product_id.to_owned());
    anyhow::ensure!(
        !active().lock().await.contains_key(&key),
        "the game is downloading; remove its unused languages once it finishes"
    );
    let install_root = crate::achv_try!(
        library
            .install_dir(&game.install_dir)
            .context("unusable install directory")
    );

    let mut removed = 0u64;
    let mut count = 0usize;
    for file in &cleanup.files {
        let Some(relative) = manifest_relative_path(&file.relative_path) else {
            tracing::warn!(
                relative = file.relative_path,
                "refusing to remove a path outside the game"
            );
            continue;
        };
        let path = install_root.join(relative);
        let size = match tokio::fs::symlink_metadata(&path).await {
            Ok(meta) if meta.is_file() => meta.len(),
            Ok(_) => {
                tracing::warn!(path = %path.display(), "not a regular file any more, leaving it");
                continue;
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => continue,
            Err(err) => {
                tracing::warn!(path = %path.display(), error = %err, "couldn't read an unused language file, leaving it");
                continue;
            }
        };
        match tokio::fs::remove_file(&path).await {
            Ok(()) => {
                removed = removed.saturating_add(size);
                count += 1;
            }
            Err(err) => tracing::warn!(path = %path.display(), error = %err, "couldn't remove an unused language file"),
        }
    }

    game.language = Some(tag.clone());
    game.size_on_disk = Some(game.size_on_disk.unwrap_or(0).saturating_sub(removed));
    crate::achv_try!(
        library
            .write_manifest(&game)
            .inspect_err(|err| tracing::error!(product_id, error = %err, "recording the game's language failed"))
            .context("recording the game's language")
    );
    write_info(&install_root, product_id, &tag, &[]).await;
    for dlc in game.dlcs.keys() {
        write_info(&install_root, dlc, &tag, &[]).await;
    }
    tracing::info!(
        product_id,
        language = tag,
        removed_files = count,
        bytes = removed,
        "removed the files of unused languages"
    );
    Ok(removed)
}

/// Writes `tag` as the language of `product_id`'s `goggame-<id>.info` in `install_root`, the way
/// GOG Galaxy records the language a game was installed in: `language` its English name and
/// `languages` the tag. Everything else in the file stays. Best-effort: a game without the file,
/// or with one that isn't a JSON object, is left as it is.
///
/// The file found among `relative_paths` wins over the one at the root, for a build that keeps it
/// in a subfolder. A depot of another language may have shipped its own copy, which this
/// overwrites with the language chosen.
pub(super) async fn write_info(install_root: &Path, product_id: &str, tag: &str, relative_paths: &[String]) {
    let name = format!("goggame-{product_id}.info");
    let relative = relative_paths
        .iter()
        .find(|path| {
            Path::new(path)
                .file_name()
                .and_then(|file| file.to_str())
                .is_some_and(|file| file.eq_ignore_ascii_case(&name))
        })
        .cloned()
        .unwrap_or(name);
    let path = install_root.join(relative);
    match update_info(&path, tag).await {
        Ok(true) => {
            tracing::info!(product_id, language = tag, path = %path.display(), "wrote the language into the game's info file")
        }
        Ok(false) => tracing::debug!(product_id, path = %path.display(), "no info file to write the language into"),
        Err(err) => {
            tracing::warn!(product_id, path = %path.display(), error = %format_args!("{err:#}"), "couldn't write the language into the game's info file");
        }
    }
}

async fn update_info(path: &Path, tag: &str) -> anyhow::Result<bool> {
    let meta = match tokio::fs::symlink_metadata(path).await {
        Ok(meta) => meta,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(err) => return Err(anyhow::Error::new(err).context("reading its metadata")),
    };
    anyhow::ensure!(meta.is_file(), "not a regular file");
    anyhow::ensure!(
        meta.len() <= INFO_MAX_BYTES,
        "{} bytes, over the {INFO_MAX_BYTES}-byte limit",
        meta.len()
    );
    let contents = crate::achv_try!(tokio::fs::read(path).await.context("reading it"));
    let mut info: serde_json::Value = crate::achv_try!(serde_json::from_slice(&contents).context("parsing it"));
    let Some(fields) = info.as_object_mut() else {
        anyhow::bail!("not a JSON object");
    };
    fields.insert(
        "language".to_owned(),
        serde_json::Value::String(crate::language::english_name(tag)),
    );
    fields.insert(
        "languages".to_owned(),
        serde_json::Value::Array(vec![serde_json::Value::String(tag.to_owned())]),
    );
    let text = crate::achv_try!(serde_json::to_vec_pretty(&info).context("encoding it"));
    let mut temporary = path.as_os_str().to_owned();
    temporary.push(".decanter-tmp");
    let temporary = std::path::PathBuf::from(temporary);
    crate::achv_try!(tokio::fs::write(&temporary, text).await.context("writing it"));
    // Renamed into place so a crash leaves either file whole, never half of one.
    if let Err(err) = tokio::fs::rename(&temporary, path).await {
        if let Err(cleanup) = tokio::fs::remove_file(&temporary).await {
            tracing::warn!(path = %temporary.display(), error = %cleanup, "couldn't remove a half-written info file");
        }
        return Err(anyhow::Error::new(err).context("replacing it"));
    }
    Ok(true)
}
