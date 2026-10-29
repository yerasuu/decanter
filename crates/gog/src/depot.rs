//! Choosing what to read from GOG Galaxy's builds and depots, which
//! [`Sessionless`]'s content-system calls fetch.

use decanter_core::GameSystem;

use crate::api::client::{Build, BuildMeta, DepotMeta, Sessionless};

/// The `system` builds of `game_id`, newest and mainline first per GOG's
/// own ordering. None at all is an error, since there is nothing to
/// install, update or sync from.
pub async fn get_builds(
    http: &Sessionless,
    access_token: &str,
    game_id: i64,
    system: GameSystem,
) -> anyhow::Result<Vec<Build>> {
    let builds = http.builds(access_token, game_id, system).await?;
    anyhow::ensure!(!builds.is_empty(), "no {system} builds available for this product");
    tracing::debug!(game_id, %system, build_count = builds.len(), "fetched gog builds");
    Ok(builds)
}

/// Mainline over any named branch (beta, etc.), falling back to whatever
/// GOG listed first — same selection gogdl makes.
pub fn select_build(builds: &[Build]) -> Option<&Build> {
    select_build_for_branch(builds, None)
}

/// The build for a specific branch — `None` for mainline, same as
/// [`select_build`] but able to land on a beta deliberately instead of
/// only ever falling back to it.
///
/// `None` back means the branch named isn't (or is no longer) offered —
/// distinct from [`select_build`]'s fallback, because silently landing on
/// a different branch than the one asked for is exactly the kind of
/// surprise a beta opt-in has to not produce.
pub fn select_build_for_branch<'a>(builds: &'a [Build], branch: Option<&str>) -> Option<&'a Build> {
    match branch {
        None => builds
            .iter()
            .find(|build| build.branch.is_none())
            .or_else(|| builds.first()),
        Some(name) => builds.iter().find(|build| build.branch.as_deref() == Some(name)),
    }
}

/// One exact build by id, regardless of branch — a manual rollback to a
/// specific historical build rather than "whichever build a branch
/// currently points at".
pub fn select_build_by_id<'a>(builds: &'a [Build], build_id: &str) -> Option<&'a Build> {
    builds.iter().find(|build| build.build_id == build_id)
}

/// Every depot in this build belonging to `product_id`.
///
/// All of them, not the first: a product's content is regularly split
/// across more than one depot, and taking only the first installs a
/// fraction of the game. And only its own — a build for a game with DLC
/// lists the DLC's depots too, and those belong to whoever installs the
/// DLC, not to the base game.
pub fn select_depots(meta: &BuildMeta, product_id: i64, system: GameSystem) -> Vec<&DepotMeta> {
    let wanted = product_id.to_string();
    meta.depots
        .iter()
        .filter(|depot| is_compatible(depot, system))
        .filter(|depot| depot.product_id.as_deref() == Some(wanted.as_str()))
        .collect()
}

/// A build lists only its own system's depots, so only Windows', which
/// can be marked by bitness, are told apart.
fn is_compatible(depot: &DepotMeta, system: GameSystem) -> bool {
    match system {
        GameSystem::Windows => depot
            .os_bitness
            .as_ref()
            .is_none_or(|bitness| bitness.iter().any(|b| b == "64" || b == "32")),
        GameSystem::MacOs => true,
    }
}
