//! The machine's own names, for the report builders to keep out of every report (decision #1034).
//!
//! Collected from what decanter already reads: the environment, the library's database, the PS
//! launch settings and Steam's login file. Blocking file and database I/O: off the UI thread.

use std::env;
use std::path::{Path, PathBuf};

use decanter_ps::bbport::LauncherSettings;
use decanter_ps::bbport::online;
use decanter_report::Identity;

/// Folders every library has, which name nothing about whose it is.
const SHARED_FOLDERS: [&str; 3] = ["decanter", "steamapps", "psport"];

const MIN_ACCOUNT_ID: usize = 6;

pub fn collect() -> Identity {
    let mut identity = Identity::default();
    for user in ["USER", "USERNAME"].into_iter().filter_map(|name| env::var(name).ok()) {
        if identity.user.is_empty() {
            identity.user = user;
        } else if identity.user != user {
            identity.extra.push(user);
        }
    }
    identity.host = crate::platform::hostname()
        .map(|host| host.trim().to_owned())
        .unwrap_or_default();

    let homes: Vec<PathBuf> = dirs::home_dir()
        .into_iter()
        .chain(
            ["HOME", "USERPROFILE"]
                .into_iter()
                .filter_map(|name| env::var_os(name).map(PathBuf::from)),
        )
        .collect();
    identity
        .home_dirs
        .extend(homes.iter().map(|home| home.display().to_string()));
    for root in decanter_core::bootstrap::known_roots() {
        identity.home_dirs.extend(under_home(&root, &homes));
    }

    profiles_and_accounts(&mut identity);
    if let Some(steam_root) = decanter_core::steam::steam_root() {
        identity
            .account_names
            .extend(decanter_core::steam::login_names(&steam_root));
    }
    let root = decanter_ps::paths::active();
    identity
        .account_names
        .extend(LauncherSettings::load(&root).text(online::NPID).map(str::to_owned));
    identity
        .pad_names
        .extend(LauncherSettings::every_text(&root, "gamepad_name"));
    identity
}

/// The folders of `root` below the home folder, which the user chose (`~/Games/John`); the ones
/// every library has are left out, since a report names them all the time.
fn under_home(root: &Path, homes: &[PathBuf]) -> Vec<String> {
    let Some(below) = homes.iter().find_map(|home| root.strip_prefix(home).ok()) else {
        return Vec::new();
    };
    below
        .components()
        .filter_map(|part| part.as_os_str().to_str())
        .filter(|part| !SHARED_FOLDERS.iter().any(|shared| shared.eq_ignore_ascii_case(part)))
        .map(str::to_owned)
        .collect()
}

fn profiles_and_accounts(identity: &mut Identity) {
    let db = match crate::core::db() {
        Ok(db) => db,
        Err(err) => {
            tracing::warn!(%err, "could not open the database for the report's identity");
            return;
        }
    };
    let profiles = match db.list_profiles() {
        Ok(profiles) => profiles,
        Err(err) => {
            tracing::warn!(%err, "could not list the profiles for the report's identity");
            return;
        }
    };
    for profile in profiles {
        match db.accounts_in_profile(&profile.id) {
            Ok(accounts) => {
                for account in accounts {
                    identity.account_names.push(account.name);
                    // A short id would hide every number that holds it; storefront ids are long.
                    if account.id.len() >= MIN_ACCOUNT_ID {
                        identity.account_names.push(account.id);
                    }
                }
            }
            Err(err) => tracing::warn!(%err, "could not list a profile's accounts for the report's identity"),
        }
        identity.extra.push(profile.name);
        identity.extra.push(profile.id);
    }
}
