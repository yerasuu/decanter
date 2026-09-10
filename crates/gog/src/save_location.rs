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

#[derive(Deserialize)]
struct RemoteConfig {
    content: RemoteConfigContent,
}

#[derive(Deserialize)]
struct RemoteConfigContent {
    #[serde(rename = "Windows")]
    windows: PlatformConfig,
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
/// under `common/`) and `prefix_drive_c` (`drive_c` of its Proton prefix).
/// Empty means Windows cloud saves are switched off for this build — there
/// is nothing to auto-detect, and the caller falls back to asking the user.
pub async fn detect(
    account_id: &str,
    product_id: &str,
    install_dir: &Path,
    prefix_drive_c: &Path,
) -> Result<Vec<SaveLocation>> {
    let game_id: i64 = crate::achv_try!(product_id.parse().context("GOG product_id must be numeric"));
    let http = crate::http_client();
    let access_token = crate::achv_try!(
        crate::with_session(account_id, |client| Ok(client.token.borrow().access_token.clone())).await
    );

    let build_client = crate::achv_try!(
        crate::cloud_saves::resolve_build_client(&http, &access_token, game_id)
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

    let cloud_storage = config.content.windows.cloud_storage;
    if !cloud_storage.enabled {
        tracing::debug!(product_id, "gog cloud saves disabled for this build");
        return Ok(Vec::new());
    }

    let users = prefix_drive_c.join("users").join("steamuser");
    let vars: HashMap<&str, PathBuf> = HashMap::from([
        ("INSTALL", install_dir.to_path_buf()),
        ("DOCUMENTS", users.join("Documents")),
        ("APPLICATION_DATA_ROAMING", users.join("AppData/Roaming")),
        ("APPLICATION_DATA_LOCAL", users.join("AppData/Local")),
        ("APPLICATION_DATA_LOCAL_LOW", users.join("AppData/LocalLow")),
        ("SAVED_GAMES", users.join("Saved Games")),
    ]);

    if cloud_storage.locations.is_empty() {
        // No explicit location declared: Galaxy falls back to this
        // client's own default storage folder.
        tracing::debug!(product_id, "resolved 1 default gog save location");
        return Ok(vec![SaveLocation {
            name: "default".to_string(),
            path: vars["APPLICATION_DATA_LOCAL"]
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
fn resolve(template: &str, vars: &HashMap<&str, PathBuf>) -> Option<PathBuf> {
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
    let sanitized = achv_runtime::sanitize_relative_path(relative.trim_start_matches('/'))?;
    Some(base.join(sanitized))
}

#[cfg(test)]
mod tests;
