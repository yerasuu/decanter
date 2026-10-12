//! Checking whether an installed game is current, and what other branches
//! GOG offers for it.
//!
//! Both read the same endpoint [`crate::depot::get_builds`] already talks
//! to for installing; nothing here downloads anything.

use anyhow::Context;
use decanter_core::{InstalledGame, Storefront};

use crate::depot::{Build, get_builds, select_build_for_branch};

/// One branch GOG offers for a game, and the build it currently points
/// at. `name: None` is mainline — the branch every game has.
#[derive(Debug, Clone)]
pub struct Branch {
    pub name: Option<String>,
    pub build_id: String,
}

/// Every branch GOG currently lists for `product_id`, mainline first.
pub async fn list_branches(account_id: &str, product_id: &str) -> anyhow::Result<Vec<Branch>> {
    let game_id: i64 = crate::achv_try!(product_id.parse().context("GOG product_id must be numeric"));
    let builds = crate::achv_try!(fetch_builds(account_id, game_id).await);

    let mut branches: Vec<Branch> = Vec::new();
    for build in &builds {
        // GOG can list more than one build per branch (history); only the
        // first — its own newest-first order — is the one anything would
        // ever install.
        if branches.iter().any(|seen| seen.name == build.branch) {
            continue;
        }
        branches.push(Branch {
            name: build.branch.clone(),
            build_id: build.build_id.clone(),
        });
    }
    // Mainline first regardless of where GOG placed it in its own
    // ordering — it's what "no branch selected" means, so it reads best
    // at the top of a list of alternatives.
    branches.sort_by_key(|branch| branch.name.is_some());
    tracing::debug!(product_id, branch_count = branches.len(), "resolved GOG branch list");
    Ok(branches)
}

/// `Some(build_id)` when the branch this game is installed on has moved
/// past the build it was installed at; `None` when it's current.
///
/// Reads the manifest for which branch to check — comparing against
/// mainline would report a beta as "outdated" forever, and comparing a
/// mainline install against a beta's build would report an update that
/// isn't one.
pub async fn check_update(account_id: &str, game: &InstalledGame) -> anyhow::Result<Option<String>> {
    let game_id: i64 = crate::achv_try!(game.game_id.parse().context("GOG product_id must be numeric"));
    let builds = crate::achv_try!(fetch_builds(account_id, game_id).await);
    let current = crate::achv_try!(
        select_build_for_branch(&builds, game.branch.as_deref()).with_context(|| match &game.branch {
            Some(name) => format!("branch {name:?} is no longer offered"),
            None => "no builds available".to_string(),
        })
    );
    let outdated = (current.build_id != game.build_id).then(|| current.build_id.clone());
    tracing::debug!(
        product_id = %game.game_id,
        outdated = outdated.is_some(),
        "checked GOG build for update"
    );
    Ok(outdated)
}

async fn fetch_builds(account_id: &str, game_id: i64) -> anyhow::Result<Vec<Build>> {
    let access_token = crate::achv_try!(
        crate::with_session(account_id, |client| {
            crate::achv_try!(
                client
                    .get_user_data()
                    .map_err(|err| anyhow::anyhow!("GOG session is not valid: {err}"))
            );
            Ok(client.token.borrow().access_token.clone())
        })
        .await
    );
    let http = crate::http_client();
    let builds = get_builds(&http, &access_token, game_id)
        .await
        .context("GOG builds lookup failed")?;
    tracing::debug!(game_id, build_count = builds.len(), "fetched GOG build list");
    Ok(builds)
}

/// Every build GOG has published for one branch of `product_id`, newest
/// first — the version history [`list_branches`] collapses down to just
/// the current build, for picking a specific one to roll back to.
///
/// `branch: None` is mainline, same as everywhere else branches are named.
pub async fn list_builds(account_id: &str, product_id: &str, branch: Option<&str>) -> anyhow::Result<Vec<Build>> {
    let game_id: i64 = crate::achv_try!(product_id.parse().context("GOG product_id must be numeric"));
    let builds = crate::achv_try!(fetch_builds(account_id, game_id).await);
    let filtered: Vec<Build> = builds
        .into_iter()
        .filter(|build| build.branch.as_deref() == branch)
        .collect();
    tracing::debug!(
        product_id,
        build_count = filtered.len(),
        "resolved GOG build list for branch"
    );
    Ok(filtered)
}

/// Checks every installed game for a profile at once, returning only the
/// ones that actually have an update — a failed lookup for one game (a
/// delisted product, a network hiccup) is skipped rather than failing the
/// whole sweep.
pub async fn check_all_updates(library: &decanter_core::Library, account_id: &str) -> Vec<(InstalledGame, String)> {
    let mut outdated = Vec::new();
    for game in library.scan(account_id).into_values() {
        if game.storefront != Storefront::Gog {
            continue;
        }
        match check_update(account_id, &game).await {
            Ok(Some(build_id)) => outdated.push((game, build_id)),
            Ok(None) => {}
            Err(err) => {
                tracing::debug!(game_id = %game.game_id, error = %format_args!("{err:#}"), "update check failed");
            }
        }
    }
    tracing::info!(
        account_id,
        outdated_count = outdated.len(),
        "checked GOG installed games for updates"
    );
    outdated
}
