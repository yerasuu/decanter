//! Where a GOG game actually keeps its saves — GOG's build manifest says
//! nothing about this, but each build's client id resolves against
//! `remote-config.gog.com` (undocumented, public, no auth needed) to a list
//! of save locations as `<?VAR?>relative/path` templates. Ported from
//! lgogdownloader's `Downloader::cloudSaveLocations`/`parseLocation`
//! (`src/main.cpp`, GPL-3.0) — the only open-source reference that
//! documents this endpoint; confirmed live against
//! `remote-config.gog.com/components/galaxy_client/clients/{id}`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::Context;
use decanter_core::GameSystem;
use serde::Deserialize;

use crate::cloud_saves::Result;

const REMOTE_CONFIG: &str = "https://remote-config.gog.com";

#[derive(Debug, Clone)]
pub struct SaveLocation {
    /// GOG's own name for this location — `"saves"`, `"settings"`, and so
    /// on. Not unique in a meaningful way, just a label to show the user
    /// when a game reports more than one.
    pub name: String,
    pub path: PathBuf,
}

/// The account folders GOG's save templates are relative to, for the
/// system whose build the game is.
pub struct UserFolders {
    /// Each `<?VAR?>` but `INSTALL`, and where it points.
    folders: Vec<(&'static str, PathBuf)>,
    /// Where Galaxy keeps a client's default storage, under
    /// `GOG.com/Galaxy/Applications/<client id>/Storage`.
    galaxy_storage: PathBuf,
    /// Whose section of remote-config applies.
    system: GameSystem,
}

impl UserFolders {
    /// A Windows account's folders, by their template names.
    fn windows(documents: PathBuf, local: PathBuf, local_low: PathBuf, roaming: PathBuf, saved_games: PathBuf) -> Self {
        Self {
            galaxy_storage: local.clone(),
            folders: vec![
                ("DOCUMENTS", documents),
                ("APPLICATION_DATA_ROAMING", roaming),
                ("APPLICATION_DATA_LOCAL", local),
                ("APPLICATION_DATA_LOCAL_LOW", local_low),
                ("SAVED_GAMES", saved_games),
            ],
            system: GameSystem::Windows,
        }
    }

    /// Wine's layout for `steamuser`, the account Proton runs every game as.
    #[inline]
    #[must_use]
    pub fn in_prefix(drive_c: &Path) -> Self {
        let users = drive_c.join("users").join("steamuser");
        Self::windows(
            users.join("Documents"),
            users.join("AppData/Local"),
            users.join("AppData/LocalLow"),
            users.join("AppData/Roaming"),
            users.join("Saved Games"),
        )
    }

    /// The signed-in user's own folders, where the system says they are: a
    /// Documents folder the user moved is followed, not assumed. `None`
    /// when the system can't name one of them.
    ///
    /// On macOS that's Documents and `~/Library/Application Support`
    /// ([`dirs::data_dir`]), where GOG puts Galaxy's storage too
    /// ([Storage](https://docs.gog.com/sdk-storage/)); GOG documents no
    /// template names, so `APPLICATION_SUPPORT` is the one Heroic reads
    /// ([save_sync.ts](https://github.com/Heroic-Games-Launcher/HeroicGamesLauncher/blob/main/src/backend/save_sync.ts)).
    #[inline]
    #[must_use]
    pub fn native() -> Option<Self> {
        match decanter_core::platform::game_system() {
            GameSystem::MacOs => {
                let (Some(documents), Some(support)) = (dirs::document_dir(), dirs::data_dir()) else {
                    return None;
                };
                Some(Self {
                    galaxy_storage: support.clone(),
                    folders: vec![("DOCUMENTS", documents), ("APPLICATION_SUPPORT", support)],
                    system: GameSystem::MacOs,
                })
            }
            GameSystem::Windows => {
                let (Some(documents), Some(local), Some(roaming), Some(home)) = (
                    dirs::document_dir(),
                    dirs::data_local_dir(),
                    dirs::data_dir(),
                    dirs::home_dir(),
                ) else {
                    return None;
                };
                // `dirs` has no getter for LocalLow or Saved Games, so both are
                // taken at Windows' default places; one the user moved isn't followed.
                let local_low = local.with_file_name("LocalLow");
                Some(Self::windows(
                    documents,
                    local,
                    local_low,
                    roaming,
                    home.join("Saved Games"),
                ))
            }
        }
    }
}

#[derive(Deserialize)]
struct RemoteConfig {
    content: RemoteConfigContent,
}

/// Heroic reads the macOS section as `MacOS`
/// ([gog.ts](https://github.com/Heroic-Games-Launcher/HeroicGamesLauncher/blob/main/src/common/types/gog.ts));
/// a build without a section for a system has no cloud saves there.
#[derive(Deserialize)]
struct RemoteConfigContent {
    #[serde(rename = "Windows", default)]
    windows: Option<PlatformConfig>,
    #[serde(rename = "MacOS", default)]
    macos: Option<PlatformConfig>,
}

#[derive(Deserialize)]
struct PlatformConfig {
    #[serde(rename = "cloudStorage")]
    cloud_storage: CloudStorageConfig,
}

#[derive(Deserialize)]
struct CloudStorageConfig {
    enabled: bool,
    #[serde(default)]
    locations: Vec<LocationEntry>,
}

#[derive(Deserialize)]
struct LocationEntry {
    name: String,
    location: String,
}

/// Resolves this game's save locations against `install_dir` (its folder
/// under `common/`) and `folders`, the account folders the game sees.
/// Empty means cloud saves are switched off for this build on the folders'
/// system — there is nothing to auto-detect, and the caller falls back to
/// asking the user.
pub async fn detect(
    account_id: &str,
    product_id: &str,
    install_dir: &Path,
    folders: &UserFolders,
) -> Result<Vec<SaveLocation>> {
    let game_id: i64 = crate::achv_try!(product_id.parse().context("GOG product_id must be numeric"));
    let http = crate::http_client();
    let access_token = crate::achv_try!(
        crate::session::live_token(account_id)
            .await
            .map(|token| token.access_token)
    );

    let build_client = crate::achv_try!(
        crate::cloud_saves::resolve_build_client(access_token.expose(), game_id)
            .await
            .inspect_err(|err| tracing::error!(product_id, error = %err, "resolving cloud saves client failed, aborting save location detection"))
    );

    let url = format!(
        "{REMOTE_CONFIG}/components/galaxy_client/clients/{}?component_version=2.0.51",
        build_client.client_id
    );
    let response = crate::achv_try!(
        http.get(&url)
            .send()
            .await
            .inspect_err(|err| tracing::error!(product_id, error = %err, "gog remote-config request failed, aborting save location detection"))
    );
    let response = crate::achv_try!(
        response
            .error_for_status()
            .inspect_err(|err| tracing::error!(product_id, error = %err, "gog remote-config lookup rejected, aborting save location detection"))
            .context("GOG remote-config lookup rejected")
    );
    let config: RemoteConfig = crate::achv_try!(response.json().await);
    tracing::debug!(product_id, "fetched gog remote-config");

    let platform_config = match folders.system {
        GameSystem::Windows => config.content.windows,
        GameSystem::MacOs => config.content.macos,
    };
    let Some(cloud_storage) = platform_config.map(|platform| platform.cloud_storage) else {
        tracing::debug!(product_id, system = %folders.system, "gog remote-config has no section for this system");
        return Ok(Vec::new());
    };
    if !cloud_storage.enabled {
        tracing::debug!(product_id, "gog cloud saves disabled for this build");
        return Ok(Vec::new());
    }

    let vars: HashMap<&str, &Path> = folders
        .folders
        .iter()
        .map(|folder| (folder.0, folder.1.as_path()))
        .chain([("INSTALL", install_dir)])
        .collect();

    if cloud_storage.locations.is_empty() {
        // No explicit location declared: Galaxy falls back to this
        // client's own default storage folder.
        tracing::debug!(product_id, "resolved 1 default gog save location");
        return Ok(vec![SaveLocation {
            name: "default".to_string(),
            path: folders
                .galaxy_storage
                .join("GOG.com/Galaxy/Applications")
                .join(&build_client.client_id)
                .join("Storage"),
        }]);
    }

    let locations: Vec<SaveLocation> = cloud_storage
        .locations
        .into_iter()
        .filter_map(|entry| {
            let Some(path) = resolve(&entry.location, &vars) else {
                return None;
            };
            Some(SaveLocation { name: entry.name, path })
        })
        .collect();
    tracing::debug!(
        product_id,
        location_count = locations.len(),
        "resolved gog save locations"
    );
    Ok(locations)
}

/// Expands one `<?VAR?>relative/path` template. `None` for a template
/// whose marker this build doesn't recognize — dropped rather than treated
/// as fatal, since another location on the same game might still resolve.
fn resolve(template: &str, vars: &HashMap<&str, &Path>) -> Option<PathBuf> {
    let Some(rest) = template.strip_prefix("<?") else {
        return None;
    };
    let Some((var_name, remainder)) = rest.split_once("?>") else {
        return None;
    };
    let Some(base) = vars.get(var_name) else {
        return None;
    };
    // Templates are written Windows-style (`\Saves`); every base we build
    // above uses `/`, so normalize before joining rather than letting a
    // literal backslash end up as one odd path component.
    let relative = remainder.replace('\\', "/");
    // This template comes from remote-config.gog.com, i.e. the network —
    // reject `..`/absolute segments rather than let one escape `base`.
    let sanitized = decanter_runtime::sanitize_relative_path(relative.trim_start_matches('/'))?;
    Some(base.join(sanitized))
}
