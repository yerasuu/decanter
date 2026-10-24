//! GOG's own product API — the fallback for everything GamesDB doesn't
//! index.
//!
//! GamesDB only covers base games. A DLC, a bundle, or a game GOG carries
//! but GamesDB never catalogued comes back either as a 404 or as a record
//! whose catalog fields are empty, which is why a third of a real library
//! ends up with no metadata at all: no release date, sometimes not even a
//! title, just the numeric id.
//!
//! `api.gog.com/products/<id>` answers for those. It is narrower than
//! GamesDB — no genres, no developer, no publisher — but it has the three
//! things that matter for an entry to be usable at all: a name, a date,
//! and what kind of product it is.
//!
//! Undocumented but long-stable (every third-party GOG client uses it), so
//! everything here is parsed loosely and a miss costs metadata rather than
//! the entry.

use serde::{Deserialize, Deserializer};

const PRODUCTS_URL: &str = "https://api.gog.com/products";

#[derive(Debug, Deserialize)]
pub struct Product {
    pub id: i64,
    #[serde(default)]
    pub title: Option<String>,
    /// GOG's own classification: `game`, `dlc`, or `pack`. More accurate
    /// than the `isGame` flag on the owned-products listing, which reports
    /// bundles as non-games.
    #[serde(default)]
    pub game_type: Option<String>,
    /// e.g. `2017-06-02T18:55:00+0200`.
    #[serde(default)]
    pub release_date: Option<String>,
    /// Whether the product has anything to install at all.
    ///
    /// `false` for goodies — artbooks, soundtracks, wallpapers. GOG calls
    /// them `dlc` like any other add-on, but they carry no installer and
    /// no depot in the game's build: they're bonus downloads (PDFs, FLAC),
    /// not game content. Offering to install one only ever fails.
    #[serde(default)]
    pub is_installable: Option<bool>,
    #[serde(default)]
    pub images: Images,
    #[serde(default)]
    pub description: Description,
    /// The DLC this product has, when asked for with
    /// `expand=expanded_dlcs`.
    ///
    /// This is the only place GOG states the relationship in its own
    /// product ids. GamesDB has a `parent_id`, but it's GamesDB's internal
    /// id and would need resolving separately.
    #[serde(default)]
    pub expanded_dlcs: Vec<Dlc>,
    /// Which systems the product's Galaxy builds are for, part of the
    /// default response ([gogapidocs](https://gogapidocs.readthedocs.io/en/latest/galaxy.html),
    /// unofficial like the endpoint itself).
    #[serde(default)]
    pub content_system_compatibility: Option<ContentSystems>,
}

/// The systems a product has Galaxy builds for; only macOS is read.
#[derive(Debug, Default, Deserialize)]
pub struct ContentSystems {
    /// A macOS build exists.
    // `default` stays because `deserialize_with` drops the implicit `None`
    // for a missing field ([serde: field attributes](https://serde.rs/field-attrs.html#deserialize_with)).
    #[serde(default, deserialize_with = "bool_or_none")]
    pub osx: Option<bool>,
}

/// One DLC, as listed on the product it belongs to.
#[derive(Debug, Deserialize)]
pub struct Dlc {
    pub id: i64,
}

#[derive(Debug, Default, Deserialize)]
pub struct Images {
    /// Wide store art, the closest thing here to a hero image.
    #[serde(default)]
    pub background: Option<String>,
    #[serde(default)]
    pub logo: Option<String>,
    #[serde(default)]
    pub icon: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
pub struct Description {
    /// The short pitch. HTML, like everything on this endpoint.
    #[serde(default)]
    pub lead: Option<String>,
}

/// How many products one request may ask for. The endpoint takes a list;
/// asking one at a time turned a few hundred entries into a few hundred
/// round trips.
pub const BATCH: usize = 50;

/// Fetches a batch of products by id, with their DLC lists.
///
/// The response silently omits ids GOG has since delisted — which is most
/// of what has no metadata — so the result is matched back by
/// [`Product::id`] rather than by position, and a short response is
/// normal rather than an error.
pub async fn fetch_many(client: &reqwest::Client, ids: &[i64]) -> Vec<Product> {
    if ids.is_empty() {
        return Vec::new();
    }
    let query = ids.iter().map(i64::to_string).collect::<Vec<_>>().join(",");

    let response = match client
        .get(PRODUCTS_URL)
        .query(&[("ids", query.as_str()), ("expand", "description,expanded_dlcs")])
        .send()
        .await
        .and_then(reqwest::Response::error_for_status)
    {
        Ok(response) => response,
        Err(err) => {
            tracing::debug!(requested = ids.len(), %err, "GOG product batch failed");
            return Vec::new();
        }
    };

    match response.json::<Vec<Product>>().await {
        Ok(products) => {
            tracing::debug!(
                requested = ids.len(),
                returned = products.len(),
                "fetched GOG product batch"
            );
            products
        }
        Err(err) => {
            tracing::debug!(requested = ids.len(), %err, "GOG product batch had an unexpected shape");
            Vec::new()
        }
    }
}

/// Turns GOG's scheme-less image path into a URL.
///
/// Unlike the owned-products listing, these paths carry no size suffix —
/// the store serves whatever the base hash points at. Some of them (a
/// logo path like `..._glx_logo.jpg`) already carry an extension; a bare
/// hash doesn't. Appending `.jpg` unconditionally doubled it on the ones
/// that already had one, which the CDN then 404s on.
pub fn image_url(path: &str) -> Option<String> {
    let path = path.trim();
    if path.is_empty() {
        return None;
    }
    let base = path.strip_prefix("//").unwrap_or(path);
    let has_extension = [".jpg", ".jpeg", ".png"].iter().any(|ext| base.ends_with(ext));
    Some(if has_extension {
        format!("https://{base}")
    } else {
        format!("https://{base}.jpg")
    })
}

/// Strips tags and unescapes the handful of entities GOG's descriptions
/// actually contain.
///
/// The field is HTML and the detail pane renders text, so without this a
/// description shows up with its markup in it. Deliberately not a real
/// parser: the alternative is an HTML dependency for one short paragraph
/// per game, and anything this mangles is still readable prose.
pub fn html_to_text(html: &str) -> String {
    let mut text = String::with_capacity(html.len());
    let mut in_tag = false;
    for c in html.chars() {
        match c {
            '<' => in_tag = true,
            // A tag boundary is a word boundary: `a<br>b` is two lines,
            // not `ab`.
            '>' => {
                in_tag = false;
                text.push(' ');
            }
            _ if in_tag => {}
            _ => text.push(c),
        }
    }

    let text = text
        .replace("&nbsp;", " ")
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'");

    // Collapse the whitespace the tag stripping just introduced.
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// A flag in any other shape reads as unknown, so that one odd field in an
/// undocumented response doesn't cost the whole product.
fn bool_or_none<'de, D>(deserializer: D) -> Result<Option<bool>, D::Error>
where
    D: Deserializer<'de>,
{
    serde_json::Value::deserialize(deserializer).map(|value| value.as_bool())
}
