//! Choosing what to read from GOG Galaxy's builds and depots, which
//! [`Sessionless`]'s content-system calls fetch.

use decanter_core::GameSystem;

use crate::api::client::{Build, BuildMeta, DepotMeta, Sessionless};
use crate::language::{Resolved, canonical, holds, is_neutral, resolve};

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

/// The depots of one product an install takes, and the language they were picked for.
pub struct DepotSelection<'meta> {
    pub depots: Vec<&'meta DepotMeta>,
    /// `None` when no language was asked for, or when the build doesn't split this product's
    /// files by language: then every depot is taken.
    pub language: Option<Resolved>,
}

/// The depots in this build belonging to `product_id`, for `languages` (best first) when given.
///
/// All of them, not the first: a product's content is regularly split
/// across more than one depot, and taking only the first installs a
/// fraction of the game. And only its own — a build for a game with DLC
/// lists the DLC's depots too, and those belong to whoever installs the
/// DLC, not to the base game.
///
/// With `languages`, the depots every language needs are kept, and of the
/// language-specific ones only those of the one language [`resolve`] picks
/// from what the build offers: a game that doesn't offer any of `languages`
/// gets English, or its first language, rather than none.
pub fn select_depots<'meta>(
    meta: &'meta BuildMeta,
    product_id: i64,
    system: GameSystem,
    languages: Option<&[String]>,
) -> DepotSelection<'meta> {
    let product = product_depots(meta, product_id, system);
    let offered = offered_in(&product);
    let language = languages.and_then(|wanted| resolve(wanted, &offered));
    let depots = match &language {
        None => product,
        Some(picked) => product
            .into_iter()
            .filter(|depot| specific(depot).is_none_or(|tags| holds(tags, &picked.tag)))
            .collect(),
    };
    DepotSelection { depots, language }
}

/// Every language `product_id`'s depots in this build are split by, as tags, in GOG's order;
/// empty when the build doesn't split them.
pub fn offered_languages(meta: &BuildMeta, product_id: i64, system: GameSystem) -> Vec<String> {
    offered_in(&product_depots(meta, product_id, system))
}

pub(crate) fn product_depots(meta: &BuildMeta, product_id: i64, system: GameSystem) -> Vec<&DepotMeta> {
    let wanted = product_id.to_string();
    meta.depots
        .iter()
        .filter(|depot| is_compatible(depot, system))
        .filter(|depot| depot.product_id.as_deref() == Some(wanted.as_str()))
        .collect()
}

/// The languages of a depot that holds files of some languages only; `None` for one every
/// language needs.
pub(crate) fn specific(depot: &DepotMeta) -> Option<&[String]> {
    depot.languages.as_deref().filter(|tags| !is_neutral(Some(tags)))
}

fn offered_in(depots: &[&DepotMeta]) -> Vec<String> {
    let mut offered: Vec<String> = Vec::new();
    for tags in depots.iter().filter_map(|depot| specific(depot)) {
        for tag in tags {
            let tag = canonical(tag);
            if !tag.is_empty() && !offered.iter().any(|seen| seen.eq_ignore_ascii_case(&tag)) {
                offered.push(tag);
            }
        }
    }
    offered
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
