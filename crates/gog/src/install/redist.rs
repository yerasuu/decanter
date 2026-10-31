//! The redistributables a GOG build names (the Visual C++ runtimes and the
//! like): fetched with the game, run once in each prefix that plays it.
//!
//! What GOG ships them as is [`crate::api::client::Dependency`]. Their files
//! go to `<root>/decanter/redist/`, in the library folder holding the game,
//! at the paths their depots give (`__redist/MSVC2015/...`), shared by every
//! game and profile of that folder. `redist.json` beside them records what
//! was fetched and which game needs which; `decanter-redist.json` in a
//! prefix records what already ran there, so it goes with the prefix when
//! it is reset.
//!
//! Nothing here fails an install or a launch: a game whose redistributable
//! is missing may still run, and one that doesn't says why in its run log.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::Context as _;
use decanter_core::GameSystem;
use decanter_core::capabilities::Feature;
use decanter_core::platform;
use decanter_runtime::journal::write_atomic;
use decanter_runtime::{Launch, Prefix, Proton};
use serde::{Deserialize, Serialize};
use tokio::io::AsyncWriteExt as _;

use super::download::{Cancel, ChunkSource, client, fetch_chunk};
use super::journal::part_path;
use super::plan::{chunk_source, manifest_relative_path};
use super::verify::verify_file;
use crate::api::client::{Dependency, SecureLinkEntry, Sessionless};

const REDIST_DIR: [&str; 2] = ["decanter", "redist"];
const RECORD: &str = "redist.json";
const PREFIX_RECORD: &str = "decanter-redist.json";

/// Only these run from the shared folder: gogdl fetches the rest into the
/// game's own directory, where the game's own installer script runs them,
/// which decanter doesn't.
const SHARED_PREFIX: &str = "__redist";

#[derive(Default, Serialize, Deserialize)]
struct Record {
    #[serde(default)]
    dependencies: BTreeMap<String, Fetched>,
    /// The ids each game's installed build named, by product id.
    #[serde(default)]
    games: BTreeMap<String, Vec<String>>,
}

#[derive(Clone, Serialize, Deserialize)]
struct Fetched {
    name: String,
    manifest: String,
    /// Relative to the redist folder, forward slashes.
    executable: String,
    arguments: String,
}

/// What already ran in one prefix, by dependency id.
#[derive(Default, Serialize, Deserialize)]
struct PrefixRecord {
    #[serde(default)]
    ran: BTreeMap<String, Ran>,
}

#[derive(Serialize, Deserialize)]
struct Ran {
    exit_code: Option<i32>,
}

/// One redistributable that didn't install in a prefix, for the run's log.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct RedistFailure {
    /// GOG's name for it.
    pub name: String,
    /// Technical English, as logs are.
    pub reason: String,
}

fn redist_dir(library_root: &Path) -> PathBuf {
    REDIST_DIR
        .iter()
        .fold(library_root.to_path_buf(), |dir, part| dir.join(part))
}

/// Whether redistributables are fetched for a `system` build here: only
/// where Windows games run in prefixes, since elsewhere they'd need the
/// user's own administrator consent for every installer.
pub(super) fn wanted(system: GameSystem) -> bool {
    system == GameSystem::Windows && platform::support(Feature::Proton).is_offered()
}

/// Fetches the redistributables `ids` names for `product_id` into
/// `library_root`'s redist folder and records that the game needs them.
/// Best-effort: every failure is logged and leaves that one out, and the
/// launch then reports it missing.
pub(super) async fn fetch(library_root: &Path, access_token: &str, product_id: &str, ids: &[String]) {
    let dir = redist_dir(library_root);
    let mut record = read_record(&dir).await;
    if ids.is_empty() {
        if record.games.remove(product_id).is_some() {
            write_record(&dir, &record, product_id).await;
        }
        return;
    }
    tracing::info!(product_id, dependencies = ?ids, "fetching the build's redistributables");
    let Some((http, repository)) = repository(access_token, product_id).await else {
        return;
    };
    let mut link = None;
    let mut needed = Vec::with_capacity(ids.len());
    for id in ids {
        let Some((dependency, executable)) = runnable(&repository, product_id, id) else {
            continue;
        };
        let is_fetched = record
            .dependencies
            .get(id)
            .is_some_and(|fetched| fetched.manifest == dependency.manifest && dir.join(&fetched.executable).is_file());
        if !is_fetched {
            if let Err(err) = fetch_dependency(&http, access_token, &mut link, &dir, dependency).await {
                tracing::warn!(product_id, dependency = id, error = %format_args!("{err:#}"), "couldn't fetch a redistributable");
                continue;
            }
            tracing::info!(product_id, dependency = id, "redistributable fetched");
        }
        record.dependencies.insert(
            id.clone(),
            Fetched {
                name: dependency.name.clone(),
                manifest: dependency.manifest.clone(),
                executable,
                arguments: dependency.arguments.clone(),
            },
        );
        needed.push(id.clone());
    }
    record.games.insert(product_id.to_owned(), needed);
    write_record(&dir, &record, product_id).await;
}

async fn repository(access_token: &str, product_id: &str) -> Option<(Sessionless, Vec<Dependency>)> {
    let http = match Sessionless::new() {
        Ok(http) => http,
        Err(err) => {
            tracing::warn!(product_id, error = %err, "couldn't build the GOG HTTP client, redistributables not fetched");
            return None;
        }
    };
    match http.dependency_repository(access_token).await {
        Ok(repository) => Some((http, repository)),
        Err(err) => {
            tracing::warn!(product_id, error = %err, "couldn't read GOG's dependency repository, redistributables not fetched");
            None
        }
    }
}

/// `id`'s entry in `repository` and its installer's path in the redist
/// folder, when it has one decanter runs.
fn runnable<'repo>(repository: &'repo [Dependency], product_id: &str, id: &str) -> Option<(&'repo Dependency, String)> {
    let Some(dependency) = repository.iter().find(|dependency| dependency.id == id) else {
        tracing::warn!(
            product_id,
            dependency = id,
            "the build names a redistributable GOG's repository doesn't list"
        );
        return None;
    };
    if dependency.executable.is_empty() {
        tracing::info!(
            product_id,
            dependency = id,
            "redistributable has nothing to run, skipped"
        );
        return None;
    }
    let Some(executable) = manifest_relative_path(&dependency.executable)
        .map(|path| path.to_string_lossy().replace('\\', "/"))
        .filter(|path| path.starts_with(SHARED_PREFIX))
    else {
        tracing::warn!(
            product_id,
            dependency = id,
            executable = dependency.executable,
            "redistributable installs into the game's directory, which decanter doesn't run, skipped"
        );
        return None;
    };
    Some((dependency, executable))
}

/// Every file of `dependency`'s depot into `dir`, each one that already
/// matches its manifest kept as it is.
async fn fetch_dependency(
    http: &Sessionless,
    access_token: &str,
    link: &mut Option<Vec<SecureLinkEntry>>,
    dir: &Path,
    dependency: &Dependency,
) -> anyhow::Result<()> {
    let files = crate::achv_try!(
        http.dependency_files(&dependency.manifest)
            .await
            .context("reading the redistributable's depot manifest")
    );
    let link = match link {
        Some(link) => link,
        None => link.insert(crate::achv_try!(
            http.dependency_link(access_token)
                .await
                .context("signing the redistributables' CDN link")
        )),
    };
    let client = crate::achv_try!(client());
    for file in files {
        let relative = crate::achv_try!(
            manifest_relative_path(&file.path)
                .with_context(|| format!("unsafe path in the depot manifest: {}", file.path))
        );
        let relative_path = relative.to_string_lossy().into_owned();
        let destination = dir.join(&relative);
        let checked = destination.clone();
        let chunks = file.chunks;
        let (is_current, chunks) = crate::achv_try!(
            tokio::task::spawn_blocking(move || (verify_file(&checked, &chunks).unwrap_or(false), chunks))
                .await
                .context("redistributable check worker panicked")
        );
        if is_current {
            continue;
        }
        let mut sources = Vec::with_capacity(chunks.len());
        for chunk in &chunks {
            sources.push(crate::achv_try!(chunk_source(
                link,
                SecureLinkEntry::redist_chunk_url,
                chunk,
                &relative_path
            )));
        }
        crate::achv_try!(
            download_file(client, &destination, &sources)
                .await
                .with_context(|| format!("{relative_path} failed to download"))
        );
    }
    Ok(())
}

/// Written to `<path>.part` and renamed once whole, so a file that is
/// there is a file that finished.
async fn download_file(client: &reqwest::Client, destination: &Path, chunks: &[ChunkSource]) -> anyhow::Result<()> {
    if let Some(parent) = destination.parent() {
        crate::achv_try!(tokio::fs::create_dir_all(parent).await);
    }
    let part = part_path(destination);
    let mut sink = crate::achv_try!(tokio::fs::File::create(&part).await);
    // Nothing stops it: a redistributable is a few megabytes, fetched while
    // the download's turn is still held.
    let cancel = Cancel::default();
    for chunk in chunks {
        let bytes = crate::achv_try!(fetch_chunk(client, chunk, &cancel).await);
        crate::achv_try!(sink.write_all(&bytes).await);
    }
    crate::achv_try!(sink.flush().await);
    crate::achv_try!(sink.sync_data().await);
    drop(sink);
    crate::achv_try!(tokio::fs::rename(&part, destination).await);
    Ok(())
}

async fn read_record(dir: &Path) -> Record {
    let path = dir.join(RECORD);
    match tokio::fs::read(&path).await {
        Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_else(|err| {
            tracing::warn!(path = %path.display(), error = %err, "unreadable redistributables record, starting a new one");
            Record::default()
        }),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Record::default(),
        Err(err) => {
            tracing::warn!(path = %path.display(), error = %err, "couldn't read the redistributables record");
            Record::default()
        }
    }
}

async fn write_record(dir: &Path, record: &Record, product_id: &str) {
    let path = dir.join(RECORD);
    let written = async {
        crate::achv_try!(tokio::fs::create_dir_all(dir).await);
        let bytes = crate::achv_try!(serde_json::to_vec_pretty(record));
        crate::achv_try!(write_atomic(&path, &bytes).await);
        anyhow::Ok(())
    }
    .await;
    if let Err(err) = written {
        tracing::warn!(product_id, path = %path.display(), error = %format_args!("{err:#}"), "couldn't write the redistributables record");
    }
}

/// Runs every redistributable `product_id` needs that hasn't run in
/// `prefix` yet, one at a time and to its end, before the game starts.
///
/// One that exited is recorded whatever its code, so it isn't run at every
/// launch: Proton hands back only the low byte of a Windows exit code, so
/// "already installed" (1638) or "reboot required" (3010) can't be told
/// from a failure, and resetting the prefix runs it again. One that couldn't
/// start, or that a signal ended (the game stopped while it ran), is not
/// recorded, and runs again next launch.
#[must_use]
#[inline]
pub async fn run_redist(library_root: &Path, product_id: &str, prefix: &Prefix, proton: &Proton) -> Vec<RedistFailure> {
    let dir = redist_dir(library_root);
    let record = read_record(&dir).await;
    let Some(ids) = record.games.get(product_id).filter(|ids| !ids.is_empty()) else {
        return Vec::new();
    };
    let record_path = prefix.path().join(PREFIX_RECORD);
    let mut ran = read_prefix_record(&record_path).await;
    let mut failures = Vec::new();
    for id in ids {
        if ran.ran.contains_key(id) {
            continue;
        }
        let (fetched, executable) = match installer(&dir, &record, product_id, id) {
            Ok(found) => found,
            Err(failure) => {
                failures.push(failure);
                continue;
            }
        };
        tracing::info!(product_id, dependency = id, prefix = %prefix.path().display(), "installing a redistributable in the prefix");
        let exit_code = match run_installer(&executable, &fetched.arguments, prefix, proton).await {
            Ok(exit_code) => exit_code,
            Err(err) => {
                tracing::warn!(product_id, dependency = id, error = %format_args!("{err:#}"), "redistributable installer couldn't run");
                failures.push(RedistFailure {
                    name: fetched.name.clone(),
                    reason: format!("{err:#}"),
                });
                continue;
            }
        };
        let Some(code) = exit_code else {
            tracing::warn!(
                product_id,
                dependency = id,
                "redistributable installer ended by a signal, it runs again next launch"
            );
            failures.push(RedistFailure {
                name: fetched.name.clone(),
                reason: "installer ended by a signal".to_owned(),
            });
            continue;
        };
        if code == 0 {
            tracing::info!(product_id, dependency = id, "redistributable installed");
        } else {
            tracing::warn!(
                product_id,
                dependency = id,
                exit_code = code,
                "redistributable installer exited with an error"
            );
            failures.push(RedistFailure {
                name: fetched.name.clone(),
                reason: format!("installer exited with {code}"),
            });
        }
        ran.ran.insert(id.clone(), Ran { exit_code });
        write_prefix_record(&record_path, &ran).await;
    }
    failures
}

/// The fetched installer of `id`, or why it can't run.
fn installer<'record>(
    dir: &Path,
    record: &'record Record,
    product_id: &str,
    id: &str,
) -> Result<(&'record Fetched, PathBuf), RedistFailure> {
    let Some(fetched) = record.dependencies.get(id) else {
        tracing::warn!(
            product_id,
            dependency = id,
            "redistributable recorded for the game but not fetched"
        );
        return Err(RedistFailure {
            name: id.to_owned(),
            reason: "not downloaded".to_owned(),
        });
    };
    let Some(executable) = manifest_relative_path(&fetched.executable).map(|relative| dir.join(relative)) else {
        tracing::warn!(
            product_id,
            dependency = id,
            executable = fetched.executable,
            "recorded redistributable installer path is unsafe, skipped"
        );
        return Err(RedistFailure {
            name: fetched.name.clone(),
            reason: format!("unsafe installer path: {}", fetched.executable),
        });
    };
    if !executable.is_file() {
        tracing::warn!(product_id, dependency = id, path = %executable.display(), "redistributable installer missing");
        return Err(RedistFailure {
            name: fetched.name.clone(),
            reason: format!("installer missing: {}", executable.display()),
        });
    }
    Ok((fetched, executable))
}

async fn write_prefix_record(path: &Path, ran: &PrefixRecord) {
    let written = async {
        let bytes = crate::achv_try!(serde_json::to_vec_pretty(ran));
        crate::achv_try!(write_atomic(path, &bytes).await);
        anyhow::Ok(())
    }
    .await;
    if let Err(err) = written {
        tracing::warn!(path = %path.display(), error = %format_args!("{err:#}"), "couldn't record the redistributable in the prefix, it runs again next launch");
    }
}

async fn read_prefix_record(path: &Path) -> PrefixRecord {
    match tokio::fs::read(path).await {
        Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_else(|err| {
            tracing::warn!(path = %path.display(), error = %err, "unreadable record of the prefix's redistributables, running them again");
            PrefixRecord::default()
        }),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => PrefixRecord::default(),
        Err(err) => {
            tracing::warn!(path = %path.display(), error = %err, "couldn't read the record of the prefix's redistributables");
            PrefixRecord::default()
        }
    }
}

/// Runs one installer in `prefix` and waits for it and whatever it left
/// running there; its exit code, `None` when a signal ended it.
///
/// An `.msi` runs through `msiexec /i`, as Wine opens nothing else as a
/// package; with no arguments of its own it gets `/qb`, so that it installs
/// without waiting for anyone to click through it, as Heroic runs the
/// repository's MSI ([setup.ts](https://github.com/Heroic-Games-Launcher/HeroicGamesLauncher/blob/main/src/backend/storeManagers/gog/setup.ts)).
async fn run_installer(
    executable: &Path,
    arguments: &str,
    prefix: &Prefix,
    proton: &Proton,
) -> anyhow::Result<Option<i32>> {
    let key = format!("{}/redist", prefix.path().display());
    let is_msi = executable
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("msi"));
    let mut launch = if is_msi {
        // Wine maps the host's `/` to `Z:`, which is how a package outside
        // the prefix is named to it.
        let package = format!("Z:{}", executable.display()).replace('/', "\\");
        Launch::new("msiexec", key).arg("/i").arg(package)
    } else {
        Launch::new(executable, key)
    };
    let arguments = split_arguments(arguments);
    if is_msi && arguments.is_empty() {
        launch.args.push("/qb".to_owned());
    }
    launch.args.extend(arguments);
    launch = launch
        .game_id("umu-decanter")
        .prefix(prefix.clone())
        .proton(proton.clone());
    launch.working_directory = executable.parent().map(Path::to_path_buf);
    let session = crate::achv_try!(decanter_runtime::start(launch).await);
    let status = crate::achv_try!(session.wait_status().await);
    Ok(status.code())
}

/// A command line split on whitespace, a double-quoted stretch kept as one
/// argument without its quotes: the shape GOG's repository writes them in
/// (`/install /quiet /norestart`).
fn split_arguments(line: &str) -> Vec<String> {
    let mut arguments = Vec::new();
    let mut current = String::new();
    let mut is_quoted = false;
    let mut has_argument = false;
    for character in line.chars() {
        match character {
            '"' => {
                is_quoted = !is_quoted;
                has_argument = true;
            }
            space if space.is_whitespace() && !is_quoted => {
                if has_argument {
                    arguments.push(core::mem::take(&mut current));
                    has_argument = false;
                }
            }
            other => {
                current.push(other);
                has_argument = true;
            }
        }
    }
    if has_argument {
        arguments.push(current);
    }
    arguments
}
