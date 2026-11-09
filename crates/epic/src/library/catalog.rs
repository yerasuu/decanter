// SPDX-License-Identifier: GPL-3.0-or-later
//! One catalog item's metadata (spec, 4.3): its title, whether it is a mod or a DLC and of what,
//! its `appName` and its art.
//!
//! The answer is read member by member from the raw JSON rather than into a fixed shape, since
//! region and locale change which members come back (spec, 13.6): a member missing or of another
//! type costs that member, never the item.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use decanter_core::secrets::Secret;
use reqwest::Method;
use serde_json::{Map, Value};
use url::form_urlencoded::Serializer;

use super::{CatalogRegion, is_id, printable};
use crate::error::CallError;
use crate::hosts::Host;
use crate::http::{self, Call, Kind};

const BULK_ITEMS: Call = Call {
    name: "catalog item",
    host: Host::Catalog,
    kind: Kind::Bearer,
    max_body: 4 << 20,
};

const TITLE_MAX_CHARS: usize = 256;
const IMAGE_URL_MAX_LEN: usize = 1024;
const MAX_IMAGES: usize = 64;
/// Bounds on what is gathered for the log, since both come from Epic's answers.
const MAX_SEEN: usize = 64;
const SEEN_NAME_MAX_LEN: usize = 64;

/// First segments of a category path that mark Unreal Engine marketplace content rather than a
/// game. UNVERIFIED guesses at Epic's category paths: the listing log reports the paths actually
/// seen so the owner's next run confirms or corrects them.
const NON_GAME_CATEGORIES: [&str; 3] = ["assets", "plugins", "projects"];

/// What one catalog item says about itself.
pub(super) struct Meta {
    pub title: Option<String>,
    /// A category `mods` (spec, 4.6 rule 2).
    pub is_mod: bool,
    /// A category path in [`NON_GAME_CATEGORIES`]: marketplace content, not a game.
    pub is_non_game: bool,
    /// `mainGameItem`, present on DLC only (spec, 4.6 rule 4).
    pub main_game: Option<MainGame>,
    /// `releaseInfo[0].appId`.
    pub app_name: Option<String>,
    pub art: Art,
}

pub(super) struct MainGame {
    /// `mainGameItem.id`, the base game's catalog item id.
    pub catalog_item_id: Option<String>,
    /// `mainGameItem.releaseInfo[0].appId`, the base game's `appName`.
    pub app_name: Option<String>,
}

#[derive(Default)]
pub(super) struct Art {
    pub icon: Option<String>,
    pub cover: Option<String>,
    pub background: Option<String>,
}

/// Member names and image types seen over a listing, logged once at its end: neither is known
/// from any reading of Epic's answers (spec, Q10), and the owner's first listing settles both.
#[derive(Default)]
pub(super) struct Seen {
    pub members: BTreeSet<String>,
    pub image_types: BTreeSet<String>,
    /// How many items carry each category path.
    pub category_paths: BTreeMap<String, usize>,
}

/// The item's raw answer, or `None` when the answer has no member for it, which legendary takes
/// for a title without metadata (spec, 4.3).
pub(super) async fn item(
    http: reqwest::Client,
    token: Arc<Secret>,
    region: Arc<CatalogRegion>,
    namespace: String,
    catalog_item_id: String,
) -> (String, Result<Option<Value>, CallError>) {
    let query = Serializer::new(String::new())
        .append_pair("id", &catalog_item_id)
        .append_pair("includeDLCDetails", "true")
        .append_pair("includeMainGameDetails", "true")
        .append_pair("country", &region.country)
        .append_pair("locale", &region.language)
        .finish();
    let path = format!("/catalog/api/shared/namespace/{namespace}/bulk/items?{query}");
    let request = http::request(&http, Method::GET, &BULK_ITEMS, &path).bearer_auth(token.expose());
    let found = http::send::<Map<String, Value>>(request, &BULK_ITEMS)
        .await
        .map(|mut items| items.remove(&catalog_item_id));
    (catalog_item_id, found)
}

pub(super) fn meta(raw: &Value, seen: &mut Seen) -> Meta {
    if let Some(object) = raw.as_object() {
        for name in object.keys() {
            remember(&mut seen.members, name);
        }
    }
    let main_game = raw
        .get("mainGameItem")
        .filter(|main| main.is_object())
        .map(|main| MainGame {
            catalog_item_id: main
                .get("id")
                .and_then(Value::as_str)
                .filter(|id| is_id(id))
                .map(str::to_owned),
            app_name: first_app_id(main),
        });
    let paths: Vec<&str> = raw
        .get("categories")
        .and_then(Value::as_array)
        .map(|categories| {
            categories
                .iter()
                .filter_map(|category| category.get("path").and_then(Value::as_str))
                .collect()
        })
        .unwrap_or_default();
    for path in &paths {
        count_path(&mut seen.category_paths, path);
    }
    Meta {
        title: raw
            .get("title")
            .and_then(Value::as_str)
            .and_then(|title| printable(title, TITLE_MAX_CHARS)),
        is_mod: paths.contains(&"mods"),
        is_non_game: paths.iter().any(|path| is_non_game_path(path)),
        main_game,
        app_name: first_app_id(raw),
        art: art(raw, seen),
    }
}

fn first_app_id(item: &Value) -> Option<String> {
    item.get("releaseInfo")?
        .as_array()?
        .first()?
        .get("appId")?
        .as_str()
        .filter(|id| is_id(id))
        .map(str::to_owned)
}

/// One image of `keyImages`, whose members (`type`, `url`, `width`, `height`) are UNVERIFIED
/// (spec, 4.3 and Q10).
struct Image<'item> {
    kind: &'item str,
    url: String,
    width: u64,
    height: u64,
}

/// The art in `keyImages`, if the item has any. The type names Epic uses are not known, so each
/// slot takes a type naming its shape first (`…Tall` for the cover, `…Wide` or the bare
/// `DieselGameBox` for the background, `…Thumbnail` for the icon, all UNVERIFIED guesses) and
/// falls back to the image's own proportions; no art is a valid outcome.
fn art(raw: &Value, seen: &mut Seen) -> Art {
    let images: Vec<Image<'_>> = raw
        .get("keyImages")
        .and_then(Value::as_array)
        .map(|images| {
            images
                .iter()
                .take(MAX_IMAGES)
                .filter_map(|image| {
                    let kind = image.get("type").and_then(Value::as_str).unwrap_or_default();
                    remember(&mut seen.image_types, kind);
                    Some(Image {
                        kind,
                        url: https_image(image.get("url").and_then(Value::as_str))?,
                        width: image.get("width").and_then(Value::as_u64).unwrap_or_default(),
                        height: image.get("height").and_then(Value::as_u64).unwrap_or_default(),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    let named = |word: &str| {
        images
            .iter()
            .find(|image| contains_ignore_case(image.kind, word))
            .map(|image| image.url.clone())
    };
    let shaped =
        |wanted: fn(&Image<'_>) -> bool| images.iter().find(|image| wanted(image)).map(|image| image.url.clone());
    let cover = named("tall").or_else(|| shaped(|image| image.height > image.width));
    Art {
        // Epic's catalog carries a `Thumbnail` for almost no title, and the sidebar list shows only the icon,
        // so a cover (cropped to the square) stands in for it.
        icon: named("thumbnail")
            .or_else(|| named("androidicon"))
            .or_else(|| cover.clone()),
        cover,
        background: named("wide")
            .or_else(|| {
                images
                    .iter()
                    .find(|image| image.kind == "DieselGameBox")
                    .map(|image| image.url.clone())
            })
            .or_else(|| shaped(|image| image.width > image.height)),
    }
}

fn contains_ignore_case(text: &str, word: &str) -> bool {
    text.to_ascii_lowercase().contains(word)
}

/// An image address the interface may fetch: HTTPS, bounded, with no control characters or
/// spaces, as Amazon's art is checked.
fn https_image(url: Option<&str>) -> Option<String> {
    let url = url?.trim();
    (url.len() <= IMAGE_URL_MAX_LEN
        && url.starts_with("https://")
        && !url.chars().any(|c| c.is_control() || c.is_whitespace()))
    .then(|| url.to_owned())
}

/// Names from Epic's answer go to the log only when they look like identifiers.
fn remember(set: &mut BTreeSet<String>, name: &str) {
    let is_name = (1..=SEEN_NAME_MAX_LEN).contains(&name.len())
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'));
    if is_name && set.len() < MAX_SEEN {
        set.insert(name.to_owned());
    }
}

fn is_non_game_path(path: &str) -> bool {
    let first = path.split('/').next().unwrap_or_default();
    NON_GAME_CATEGORIES.iter().any(|name| first.eq_ignore_ascii_case(name))
}

/// Paths come from Epic's answers and go to the log, so they are bounded like [`remember`]'s names.
fn count_path(paths: &mut BTreeMap<String, usize>, path: &str) {
    let is_path = (1..=SEEN_NAME_MAX_LEN).contains(&path.len())
        && path
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.' | b'/'));
    if !is_path {
        return;
    }
    if let Some(count) = paths.get_mut(path) {
        *count = count.saturating_add(1);
    } else if paths.len() < MAX_SEEN * 8 {
        paths.insert(path.to_owned(), 1);
    }
}
