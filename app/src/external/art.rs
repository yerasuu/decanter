//! Finding art for an external game by its name: SteamGridDB first, the
//! community's library-art catalog, then Steam's own store art as a second
//! source that needs no key — most PC games are on Steam even when the copy
//! being added isn't.
//!
//! SteamGridDB wants a personal API key (free, from the account's
//! preferences page), kept in the OS keyring next to the storefront
//! sessions. Without one only Steam is searched.

use std::sync::LazyLock;

use decanter_core::Storefront;
use rust_i18n::t;
use serde::Deserialize;

static CLIENT: LazyLock<reqwest::Client> = LazyLock::new(reqwest::Client::new);

const SGDB_API: &str = "https://www.steamgriddb.com/api/v2";
/// Where someone signed in to SteamGridDB generates their key.
pub const SGDB_KEY_PAGE: &str = "https://www.steamgriddb.com/profile/preferences/api";
/// The keyring "account" the key is filed under: not a storefront account,
/// but the keyring entry is keyed the same way.
const SGDB_KEYRING_ACCOUNT: &str = "steamgriddb";

const STEAM_SEARCH: &str = "https://store.steampowered.com/api/storesearch/";
/// Steam's own store API has no art paths for library assets; this public
/// mirror of the PICS app info does.
const STEAM_APP_INFO: &str = "https://api.steamcmd.net/v1/info";
const STEAM_ASSETS: &str = "https://shared.akamai.steamstatic.com/store_item_assets/steam/apps";
const STEAM_ICONS: &str = "https://cdn.cloudflare.steamstatic.com/steamcommunity/public/images/apps";

/// Store apps whose art is offered from one Steam search — enough to cover
/// a name shared by a game and its sequel or remaster.
const STEAM_APPS: usize = 3;
/// Images offered per kind from SteamGridDB, in its own score order.
const SGDB_PER_KIND: usize = 12;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Source {
    SteamGridDb,
    Steam,
}

#[derive(Clone)]
pub struct Candidate {
    pub url: String,
    pub source: Source,
}

#[derive(Default)]
pub struct Candidates {
    pub covers: Vec<Candidate>,
    pub heroes: Vec<Candidate>,
    pub icons: Vec<Candidate>,
    /// One line per source that failed, shown under the results: a bad
    /// SteamGridDB key shouldn't hide what Steam did find.
    pub problems: Vec<String>,
}

impl Candidates {
    pub fn is_empty(&self) -> bool {
        self.covers.is_empty() && self.heroes.is_empty() && self.icons.is_empty()
    }
}

/// The saved SteamGridDB key, if any. Blocking: a keyring round trip.
pub fn api_key() -> Option<String> {
    decanter_core::load_session(SGDB_KEYRING_ACCOUNT, Storefront::Unspecified)
        .inspect_err(|err| tracing::warn!(%err, "could not read the steamgriddb key from the keyring"))
        .ok()
        .flatten()
}

/// Saves the key, or forgets it when `key` is empty. Blocking.
pub fn set_api_key(key: &str) -> Result<(), String> {
    let result = if key.is_empty() {
        decanter_core::delete_session(SGDB_KEYRING_ACCOUNT, Storefront::Unspecified)
    } else {
        decanter_core::save_session(SGDB_KEYRING_ACCOUNT, Storefront::Unspecified, key)
    };
    result
        .inspect(|()| tracing::info!(cleared = key.is_empty(), "steamgriddb key updated"))
        .map_err(|err| t!("external.sources.errors.keyring", error = err).into_owned())
}

/// Searches every source for `name`. Never fails as a whole: whatever a
/// source couldn't do lands in [`Candidates::problems`].
pub async fn search(name: &str) -> Candidates {
    let key = tokio::task::spawn_blocking(api_key).await.ok().flatten();
    let (sgdb, steam) = futures::join!(
        async {
            match key {
                Some(key) => Some(search_sgdb(&key, name).await),
                None => None,
            }
        },
        search_steam(name)
    );
    let mut found = Candidates::default();
    match sgdb {
        Some(Ok(sgdb)) => found = sgdb,
        Some(Err(err)) => found.problems.push(format!("SteamGridDB: {err}")),
        None => found
            .problems
            .push(format!("SteamGridDB: {}", t!("external.art.no_key"))),
    }
    match steam {
        Ok(steam) => {
            found.covers.extend(steam.covers);
            found.heroes.extend(steam.heroes);
            found.icons.extend(steam.icons);
        }
        Err(err) => found.problems.push(format!("Steam: {err}")),
    }
    tracing::debug!(
        name,
        covers = found.covers.len(),
        heroes = found.heroes.len(),
        icons = found.icons.len(),
        problems = found.problems.len(),
        "external art search finished"
    );
    found
}

#[derive(Deserialize)]
struct SgdbResponse<T> {
    // A path rather than plain `default`, which would demand `T: Default`.
    #[serde(default = "Vec::new")]
    data: Vec<T>,
}

#[derive(Deserialize)]
struct SgdbGame {
    id: u64,
}

#[derive(Deserialize)]
struct SgdbImage {
    url: String,
}

async fn get_json<T: serde::de::DeserializeOwned>(request: reqwest::RequestBuilder) -> Result<T, String> {
    let response = request
        .send()
        .await
        .map_err(|err| t!("external.art.errors.offline", error = err).into_owned())?;
    let status = response.status();
    if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
        return Err(t!("external.art.errors.bad_key").into_owned());
    }
    let response = response
        .error_for_status()
        .map_err(|err| t!("external.art.errors.status", status = status, error = err).into_owned())?;
    let bytes = response
        .bytes()
        .await
        .map_err(|err| t!("external.art.errors.truncated", error = err).into_owned())?;
    serde_json::from_slice(&bytes).map_err(|err| t!("external.art.errors.unexpected", error = err).into_owned())
}

fn sgdb_url(segments: &[&str], query: &[(&str, &str)]) -> Result<reqwest::Url, String> {
    let mut url = reqwest::Url::parse(SGDB_API).map_err(|err| err.to_string())?;
    url.path_segments_mut()
        .map_err(|()| t!("external.art.errors.base_url").into_owned())?
        .extend(segments);
    url.query_pairs_mut().extend_pairs(query);
    Ok(url)
}

async fn search_sgdb(key: &str, name: &str) -> Result<Candidates, String> {
    let request = |url: reqwest::Url| CLIENT.get(url).bearer_auth(key);
    let games: SgdbResponse<SgdbGame> = get_json(request(sgdb_url(&["search", "autocomplete", name], &[])?)).await?;
    let Some(game) = games.data.first() else {
        return Err(t!("external.art.errors.unknown_game", name = name).into_owned());
    };
    let id = game.id.to_string();
    // Without `nsfw`/`humor` set, SteamGridDB includes both.
    let safe = [("nsfw", "false"), ("humor", "false")];
    let (covers, heroes, icons) = futures::join!(
        get_json::<SgdbResponse<SgdbImage>>(request(sgdb_url(
            &["grids", "game", &id],
            &[safe[0], safe[1], ("dimensions", "600x900,342x482,660x930")]
        )?)),
        get_json::<SgdbResponse<SgdbImage>>(request(sgdb_url(&["heroes", "game", &id], &safe)?)),
        get_json::<SgdbResponse<SgdbImage>>(request(sgdb_url(&["icons", "game", &id], &safe)?)),
    );
    let candidates = |images: Result<SgdbResponse<SgdbImage>, String>| -> Result<Vec<Candidate>, String> {
        Ok(images?
            .data
            .into_iter()
            .take(SGDB_PER_KIND)
            .map(|image| Candidate {
                url: image.url,
                source: Source::SteamGridDb,
            })
            .collect())
    };
    Ok(Candidates {
        covers: candidates(covers)?,
        heroes: candidates(heroes)?,
        icons: candidates(icons)?,
        problems: Vec::new(),
    })
}

#[derive(Deserialize)]
struct SteamSearch {
    #[serde(default)]
    items: Vec<SteamSearchItem>,
}

#[derive(Deserialize)]
struct SteamSearchItem {
    id: u64,
}

async fn search_steam(name: &str) -> Result<Candidates, String> {
    let url = reqwest::Url::parse_with_params(STEAM_SEARCH, &[("term", name), ("cc", "us"), ("l", "english")])
        .map_err(|err| err.to_string())?;
    let search: SteamSearch = get_json(CLIENT.get(url)).await?;
    if search.items.is_empty() {
        return Err(t!("external.art.errors.no_game", name = name).into_owned());
    }
    let apps = futures::future::join_all(search.items.iter().take(STEAM_APPS).map(|item| steam_app(item.id))).await;
    let mut found = Candidates::default();
    for app in apps {
        match app {
            Ok(app) => {
                found.covers.extend(app.cover);
                found.heroes.extend(app.hero);
                found.icons.extend(app.icon);
            }
            // One app's details missing still leaves the others' art.
            Err(err) => tracing::warn!(%err, "could not read steam app info"),
        }
    }
    Ok(found)
}

struct SteamApp {
    cover: Option<Candidate>,
    hero: Option<Candidate>,
    icon: Option<Candidate>,
}

/// The art Steam's library itself shows for `appid`, read from its app
/// info — since 2024 the paths carry a content hash, so they can't be
/// guessed from the id alone.
async fn steam_app(appid: u64) -> Result<SteamApp, String> {
    let info: serde_json::Value = get_json(CLIENT.get(format!("{STEAM_APP_INFO}/{appid}"))).await?;
    let common = &info["data"][appid.to_string()]["common"];
    let asset = |kind: &str, size: &str| {
        common["library_assets_full"][kind][size]["english"]
            .as_str()
            .map(|path| Candidate {
                url: format!("{STEAM_ASSETS}/{appid}/{path}"),
                source: Source::Steam,
            })
    };
    Ok(SteamApp {
        cover: asset("library_capsule", "image2x").or_else(|| asset("library_capsule", "image")),
        hero: asset("library_hero", "image"),
        icon: common["icon"].as_str().map(|hash| Candidate {
            url: format!("{STEAM_ICONS}/{appid}/{hash}.jpg"),
            source: Source::Steam,
        }),
    })
}
