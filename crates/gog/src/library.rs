//! The GOG library, fetched by us directly rather than through an
//! external plugin registry chain that would just reach the same public
//! endpoints this module calls itself — achv owns the whole path, and
//! nothing else needs to be running for it to work.
//!
//! Four sources, merged:
//!
//! - `getFilteredProducts`, sorted by `date_purchased` — GOG's own view of
//!   what you own: title, art and product category for every entry, in
//!   the order GOG itself considers them added. Authoritative on both
//!   counts: the only source that covers products GamesDB has never heard
//!   of, and (unlike `orders.rs`'s order history) the only one that
//!   orders gifts and dev grants alongside real purchases instead of
//!   omitting them. `sortBy` isn't in the `gog` crate's typed
//!   `FilterParam`, so this bypasses it for a raw request the same way
//!   `orders.rs` already does for the orders endpoint.
//! - `get_games` — the raw owned-id list. Kept because it can include ids
//!   the filtered view omits; those would otherwise silently vanish.
//! - `orders.rs` — a real calendar date for whatever went through an
//!   actual order. Narrower than the sort above (silent on gifts and dev
//!   grants) but a true date rather than a rank, kept as a fallback for
//!   when a cycle's ranked fetch fails outright.
//! - GamesDB — richer catalog data (developer, publisher, genres, release
//!   date, portrait cover) for the entries it does know.
//!
//! `install_state` is none of the above: it's local, read from this
//! profile's appmanifest.
//!
//! Fetched in two passes, because the first is seconds and the second is
//! a minute. [`list_games`] returns what GOG's own listing already knows —
//! title, art, category — a page at a time, so the library is on screen
//! and in the cache almost immediately. [`enrich_games`] then layers
//! GamesDB and the order history on top, many at a time, updating entries
//! in place. Closing the app between the two costs the metadata, not the
//! library.

use std::collections::{HashMap, HashSet};

use achv_core::Storefront;
use achv_core::{Game, InstallState};
use anyhow::Context;
use futures_util::StreamExt;
use serde::Deserialize;

use crate::gamesdb::{self, Image, Localized};

const FILTERED_PRODUCTS_URL: &str = "https://embed.gog.com/account/getFilteredProducts";

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct FilteredProductsPage {
    #[serde(default)]
    products: Vec<gog::gog::ProductDetails>,
    #[serde(default, rename = "totalPages")]
    total_pages: i64,
}

/// Every owned product (games, extras, movies, gifts, dev grants alike),
/// in the order GOG itself reports them added — index 0 is the most
/// recent. A raw request rather than the crate's typed
/// `get_all_filtered_products`: `sortBy` isn't one of the params
/// `FilterParam` models, and this is otherwise the exact same endpoint,
/// same `mediaType=1` filter, same response shape, so it reuses the
/// crate's own `ProductDetails` for deserializing.
///
/// Undocumented sort parameter, so a response shape surprise degrades to
/// an error the caller can shrug off (see `list_games`), not a panic.
async fn each_product_page(
    access_token: &str,
    mut on_page: impl AsyncFnMut(Vec<gog::gog::ProductDetails>),
) -> anyhow::Result<()> {
    let http = crate::http_client();
    let mut page = 1;

    loop {
        let response = crate::achv_try!(
            http.get(FILTERED_PRODUCTS_URL)
                .query(&[
                    ("mediaType", "1"),
                    ("sortBy", "date_purchased"),
                    ("page", &page.to_string()),
                ])
                .bearer_auth(access_token)
                .header("CSRF", "csrf=true")
                .send()
                .await
        );
        let response = crate::achv_try!(
            response
                .error_for_status()
                .context("GOG library listing request rejected")
        );
        let body = crate::achv_try!(response.text().await);

        let parsed: FilteredProductsPage = crate::achv_try!(serde_json::from_str(&body).with_context(|| {
            format!(
                "GOG's filtered-products response had an unexpected shape; first bytes: {}",
                body.chars().take(300).collect::<String>()
            )
        }));
        tracing::debug!(
            page,
            total_pages = parsed.total_pages,
            product_count = parsed.products.len(),
            "fetched GOG filtered-products page"
        );

        let is_last_page = page >= parsed.total_pages.max(1);
        on_page(parsed.products).await;
        if is_last_page {
            return Ok(());
        }
        page += 1;
    }
}

/// What kind of product an entry is, for filtering a library that mixes
/// games with the DLC, soundtracks and bonus content GOG grants alongside
/// them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GameKind {
    Game,
    Movie,
    /// DLC, soundtracks, artbooks — anything GOG doesn't call a game.
    Extra,
}

#[derive(Debug, Clone, PartialEq)]
pub struct LibraryEntry {
    pub game: Game,
    pub kind: GameKind,
    /// When this was bought, from GOG's order history. `None` for the
    /// sizeable share of a library that never went through an order —
    /// giveaways, dev grants, bundle re-grants.
    pub purchased_at: Option<i64>,
    /// Position in GOG's own "added to library" order, 0 = most recent.
    /// Unlike `purchased_at`, this covers gifts and dev grants too — it's
    /// `None` only when the ranked fetch itself failed this cycle, not
    /// because this particular game lacks an order.
    pub library_rank: Option<i64>,
    /// Whether this has anything to install.
    ///
    /// `true` until GOG says otherwise, because that's the common case and
    /// the products API is the only thing that knows. `false` marks the
    /// goodies — artbooks, soundtracks — which are owned content but not
    /// game content.
    pub installable: bool,
    /// The product this one is a DLC of, if it is one.
    ///
    /// Discovered from the parent's own product record, which is where GOG
    /// states the relationship — a DLC's record says nothing about what it
    /// belongs to.
    pub parent_id: Option<String>,
}

/// GOG returns image paths without a scheme or extension, sized by
/// suffix. 196px is the square product shot the sidebar wants.
fn product_image(image: &str, suffix: &str) -> Option<String> {
    let image = image.trim();
    if image.is_empty() {
        return None;
    }
    let base = image.strip_prefix("//").unwrap_or(image);
    Some(format!("https://{base}{suffix}.jpg"))
}

/// A group of entries ready to show. Emitted as they resolve rather than
/// returned all at once — see the module docs.
pub type Batch = Vec<LibraryEntry>;

/// How many entries the second pass resolves at a time. GamesDB answers
/// one request in well under a tenth of a second, so this is about not
/// opening a connection per game rather than about GamesDB's limits.
const ENRICH_CONCURRENCY: usize = 8;

/// How many enriched entries accumulate before being handed over. One
/// update per game would mean a re-render per game; a group means the
/// list fills in visibly without the UI doing 700 of them.
const ENRICH_BATCH: usize = 24;

/// Pass one: everything GOG's own listing already knows — title, art,
/// category, and the order it considers them added.
///
/// Each page is handed to `on_batch` as it lands, so the first games are
/// on screen while the rest are still being fetched. The full list comes
/// back too, for [`enrich_games`] to work through.
///
/// This is the pass that must not fail: it's the library itself.
pub async fn list_games(account_id: &str, on_batch: &mut impl FnMut(Batch)) -> anyhow::Result<Vec<LibraryEntry>> {
    let (owned_ids, access_token) = crate::achv_try!(
        crate::with_session(account_id, |client| {
            let owned_ids = crate::achv_try!(
                client
                    .get_games()
                    .map_err(|err| anyhow::anyhow!("gog owned-games request failed: {err}"))
            );
            tracing::debug!(owned_id_count = owned_ids.len(), "fetched GOG owned-games id list");
            Ok((owned_ids, client.token.borrow().access_token.clone()))
        })
        .await
    );

    let mut entries: Vec<LibraryEntry> = Vec::new();
    let mut seen: HashSet<i64> = HashSet::new();

    // Position across all pages *is* GOG's added-to-library order, so the
    // rank is the running count rather than an index within a page.
    let paged = each_product_page(&access_token, async |products| {
        let start = entries.len();
        let batch: Batch = products
            .into_iter()
            .enumerate()
            .map(|(index, product)| {
                seen.insert(product.id);
                let mut entry = base_entry(product.id, Some(&product));
                entry.library_rank = Some((start + index) as i64);
                entry
            })
            .collect();
        entries.extend(batch.iter().cloned());
        on_batch(batch);
    })
    .await;

    // A failure here costs metadata and ordering, not the library — the
    // owned-id sweep below still covers everything.
    if let Err(err) = paged {
        tracing::warn!("gog product listing failed: {err}");
    }

    // Owned but absent from the filtered view: that's what a non-game
    // grant looks like from here. They still belong in the library, just
    // without GOG-side metadata.
    let leftovers: Batch = owned_ids
        .into_iter()
        .filter(|id| !seen.contains(id))
        .map(|id| base_entry(id, None))
        .collect();
    if !leftovers.is_empty() {
        tracing::debug!(
            leftover_count = leftovers.len(),
            "found owned products absent from GOG's filtered listing"
        );
        entries.extend(leftovers.iter().cloned());
        on_batch(leftovers);
    }

    let result: Vec<LibraryEntry> = entries
        .into_iter()
        .map(|entry| with_install_state(account_id, entry))
        .collect();
    tracing::info!(account_id, game_count = result.len(), "GOG library listing finished");
    Ok(result)
}

/// Pass two: purchase dates and GamesDB catalog data, layered onto what
/// [`list_games`] already returned.
///
/// Nothing here is load-bearing. Every source is best-effort, and an entry
/// that gains nothing is still the entry the first pass produced — which
/// is why this can run long after the library is already usable.
///
/// `on_progress(done, total)` fires after each entry's GamesDB lookup
/// resolves — the one step here slow enough (a request per game) that a
/// caller showing a progress bar needs more than "it's still going".
pub async fn enrich_games(
    account_id: &str,
    entries: Vec<LibraryEntry>,
    on_batch: &mut impl FnMut(Batch),
    on_progress: &mut impl FnMut(usize, usize),
) {
    tracing::debug!(
        account_id,
        entry_count = entries.len(),
        "starting GOG library enrichment pass"
    );

    // Started here, awaited at the end. The order history is its own
    // paginated fetch and used to run first, which left ~20 seconds where
    // the library was up and nothing visible was changing — it only feeds
    // a sort fallback, so it has no business gating the catalog data.
    let profile = account_id.to_string();
    let dates = tokio::spawn(async move { crate::orders::purchase_dates(&profile).await });

    let http = crate::http_client();

    // Step one: GamesDB, which is per-id and has no batch endpoint, so
    // several at a time. Entries it actually indexed go out as they land;
    // the rest are held back for step two rather than being emitted
    // unchanged, which would be a cache write per game for no change.
    let total = entries.len();
    let mut resolved = futures_util::stream::iter(entries.into_iter().map(|mut entry| {
        let http = &http;
        async move {
            let indexed = match entry.game.id.parse::<i64>() {
                Ok(id) => apply_gamesdb(&mut entry.game, id, http).await,
                Err(_) => false,
            };
            (entry, indexed)
        }
    }))
    // Ordered, not `buffer_unordered`: entries keep the order GOG reports
    // them added, which is what the "recently added" sort reads when a
    // purchase date is missing.
    .buffered(ENRICH_CONCURRENCY);

    let mut all: Vec<LibraryEntry> = Vec::new();
    let mut unindexed: HashSet<usize> = HashSet::new();
    let mut batch: Batch = Vec::with_capacity(ENRICH_BATCH);
    while let Some((entry, indexed)) = resolved.next().await {
        // Fine-grained per-entry step inside a stream that can churn through
        // hundreds of games — trace, not debug, so it doesn't drown out
        // everything else at debug level on a real library.
        tracing::trace!(product_id = %entry.game.id, indexed, "resolved GamesDB lookup for library entry");
        if indexed {
            batch.push(entry.clone());
            if batch.len() == ENRICH_BATCH {
                on_batch(std::mem::take(&mut batch));
                batch.reserve(ENRICH_BATCH);
            }
        } else {
            unindexed.insert(all.len());
        }
        all.push(entry);
        on_progress(all.len(), total);
    }
    if !batch.is_empty() {
        on_batch(batch);
    }

    apply_products(&mut all, &unindexed, &http, on_batch).await;

    // Best-effort: an undocumented endpoint going quiet should cost the
    // purchase dates, not the library. Narrower than the ranking from
    // pass one (silent on gifts and dev grants) but a real calendar date,
    // so it's kept as a fallback for whatever cycle's ranked fetch came
    // back empty.
    let purchased = match dates.await {
        Ok(Ok(purchased)) => purchased,
        Ok(Err(err)) => {
            tracing::warn!("GOG order history unavailable: {err}");
            tracing::info!(
                account_id,
                game_count = all.len(),
                "GOG library enrichment finished without purchase dates"
            );
            return;
        }
        Err(err) => {
            tracing::warn!("GOG order history task panicked: {err}");
            tracing::info!(
                account_id,
                game_count = all.len(),
                "GOG library enrichment finished without purchase dates"
            );
            return;
        }
    };

    let game_count = all.len();
    // Only the entries that actually gained a date — re-emitting the rest
    // would be a second cache write per game for no change.
    let dated: Batch = all
        .into_iter()
        .filter_map(|mut entry| {
            entry.purchased_at = purchased.get(&entry.game.id).copied();
            entry.purchased_at.is_some().then_some(entry)
        })
        .collect();
    let dated_count = dated.len();
    if !dated.is_empty() {
        on_batch(dated);
    }
    tracing::info!(account_id, game_count, dated_count, "GOG library enrichment finished");
}

/// Step two: GOG's own product API for everything GamesDB didn't index —
/// DLC, bundles, and games it never catalogued.
///
/// Asked in batches of [`product::BATCH`]. One request per entry turned a
/// few hundred unindexed products into a few hundred round trips, which
/// on a real library was the slowest thing in the whole fetch.
async fn apply_products(
    all: &mut [LibraryEntry],
    unindexed: &HashSet<usize>,
    http: &reqwest::Client,
    on_batch: &mut impl FnMut(Batch),
) {
    // Every entry, not just the unindexed ones: a base game GamesDB
    // covered perfectly is still the only place its DLC are listed.
    let every: Vec<usize> = (0..all.len()).collect();
    let mut parents: HashMap<String, String> = HashMap::new();

    for chunk in every.chunks(crate::product::BATCH) {
        // A delisted product is simply absent from the response, so
        // results are matched back by id rather than by position.
        let mut wanted: HashMap<String, usize> = HashMap::new();
        let mut ids = Vec::with_capacity(chunk.len());
        for index in chunk {
            if let Ok(id) = all[*index].game.id.parse::<i64>() {
                ids.push(id);
                wanted.insert(id.to_string(), *index);
            }
        }

        let products = crate::product::fetch_many(http, &ids).await;
        let mut batch: Batch = Vec::new();
        for product in products {
            let Some(index) = wanted.get(&product.id.to_string()).copied() else {
                continue;
            };
            for dlc in &product.expanded_dlcs {
                parents.insert(dlc.id.to_string(), product.id.to_string());
            }

            // The category correction applies to every entry, indexed or
            // not: the owned-products listing reports plenty of real games
            // as `isGame: false`, and GamesDB having catalogued one says
            // nothing about that.
            let mut changed = apply_product_facts(&mut all[index], &product);
            // Metadata, on the other hand, only fills holes GamesDB left.
            if unindexed.contains(&index) {
                apply_product(&mut all[index], product);
                changed = true;
            }
            if changed {
                batch.push(all[index].clone());
            }
        }
        if !batch.is_empty() {
            on_batch(batch);
        }
    }

    // Assigned only once every batch is in: a DLC's parent may well be in
    // a later chunk than the DLC itself.
    let mut reparented: Batch = Vec::new();
    for entry in all.iter_mut() {
        let Some(parent) = parents.get(&entry.game.id) else {
            continue;
        };
        if entry.parent_id.as_deref() != Some(parent.as_str()) {
            entry.parent_id = Some(parent.clone());
            reparented.push(entry.clone());
        }
    }
    if !reparented.is_empty() {
        on_batch(reparented);
    }
}

pub async fn find_game(account_id: &str, product_id: &str) -> anyhow::Result<Game> {
    // Ignored: this only needs the finished list, not the progress.
    crate::achv_try!(list_games(account_id, &mut |_| {}).await)
        .into_iter()
        .map(|entry| entry.game)
        .find(|game| game.id == product_id)
        .with_context(|| format!("product {product_id} not found in profile {account_id}'s library"))
}

/// Everything an entry can be without touching GamesDB: GOG's own product
/// data, or just the id when even that is missing.
///
/// This is what pass one emits, and it is already enough to render a game
/// in the library — title, art and category.
fn base_entry(id: i64, product: Option<&gog::gog::ProductDetails>) -> LibraryEntry {
    let kind = match product {
        Some(product) if product.is_movie => GameKind::Movie,
        Some(product) if product.is_game => GameKind::Game,
        // Neither a movie nor a game — includes owned-but-absent-from-
        // GOG's-listing, which is what a non-game grant looks like here.
        Some(_) | None => GameKind::Extra,
    };

    LibraryEntry {
        game: Game {
            id: id.to_string(),
            title: product
                .map(|product| product.title.clone())
                .unwrap_or_else(|| id.to_string()),
            storefront: Storefront::Gog,
            icon_url: product.and_then(|product| product_image(&product.image, "_196")),
            cover_url: product.and_then(|product| product_image(&product.image, "_392")),
            ..Default::default()
        },
        kind,
        purchased_at: None,
        library_rank: None,
        installable: true,
        parent_id: None,
    }
}

/// Layers GamesDB's richer catalog data over what GOG already said.
///
/// GOG's own data is the floor and is never cleared: an entry GamesDB has
/// never heard of keeps its title and art rather than being reduced to a
/// bare numeric id.
/// Returns whether GamesDB actually had catalog data. A record with an
/// empty `game` object is what a DLC looks like here — present, but with
/// nothing on it, which is not the same as being indexed.
async fn apply_gamesdb(game: &mut Game, id: i64, http: &reqwest::Client) -> bool {
    // trace, not debug, since a single enrichment pass can make hundreds of
    // these calls — at debug level they'd drown out everything else.
    let Ok(release) = gamesdb::fetch_release(http, id)
        .await
        .inspect_err(|err| tracing::trace!("GamesDB has no release for {id}: {err}"))
    else {
        return false;
    };

    // GamesDB indexes by canonical release, not by GOG's own SKU — an
    // edition variant ("Agony: Unrated Edition") and its base game can
    // resolve to the same GamesDB record, so trusting this title always
    // silently collapses two distinct store listings down to one name.
    // Only used to fill the placeholder `base_entry` left when GOG's own
    // product data didn't have a title at all.
    if game.title == id.to_string()
        && let Some(title) = release.title.resolve()
    {
        game.title = title;
    }
    game.description = release.summary.as_ref().and_then(Localized::resolve);
    game.icon_url = release.icon.as_ref().map(Image::resolve).or(game.icon_url.take());
    // GamesDB's portrait crop is a real cover; GOG's product shot is
    // square, so prefer the former when it exists.
    game.cover_url = release
        .game
        .vertical_cover
        .as_ref()
        .or(release.game.cover.as_ref())
        .map(Image::resolve)
        .or(game.cover_url.take());
    game.background_url = release.game.background.as_ref().map(Image::resolve);
    game.developer = release.game.developers.first().map(|c| c.name.clone());
    game.publisher = release.game.publishers.first().map(|c| c.name.clone());
    game.genres = release
        .game
        .genres
        .iter()
        .filter_map(|genre| genre.name.resolve())
        .collect();
    game.release_date = release
        .game
        .first_release_date
        .as_deref()
        .and_then(gamesdb::parse_release_date);
    game.rating = release.game.aggregated_rating.map(|score| score.to_string());

    game.release_date.is_some() || !game.genres.is_empty()
}

/// Fills in from GOG's own product API what GamesDB had nothing to say
/// about, and corrects the entry's category while it's there.
///
/// Only ever adds. GOG's owned-products listing already supplied a title
/// and art for most entries, and this must not overwrite a good value
/// with a worse one — the exception is a title that is still just the
/// numeric id, which is not a title at all.
fn apply_product(entry: &mut LibraryEntry, product: crate::product::Product) {
    if let Some(title) = product.title.filter(|title| !title.trim().is_empty())
        && entry.game.title == entry.game.id
    {
        entry.game.title = title;
    }
    if entry.game.description.is_none()
        && let Some(lead) = product.description.lead
    {
        let text = crate::product::html_to_text(&lead);
        entry.game.description = (!text.is_empty()).then_some(text);
    }
    if entry.game.release_date.is_none() {
        entry.game.release_date = product.release_date.as_deref().and_then(gamesdb::parse_release_date);
    }
    if entry.game.background_url.is_none() {
        entry.game.background_url = product.images.background.as_deref().and_then(crate::product::image_url);
    }
    if entry.game.icon_url.is_none() {
        entry.game.icon_url = product
            .images
            .icon
            .as_deref()
            .or(product.images.logo.as_deref())
            .and_then(crate::product::image_url);
    }
}

/// Applies the two facts only GOG's product record carries — what kind of
/// product this is, and whether it has anything to install — returning
/// whether either actually changed.
///
/// The category correction matters most: the owned-products listing
/// reports bundles, and a fair number of plain games, as `isGame: false`,
/// so they land as extras — which hides them from the default library view
/// and refuses to install them.
fn apply_product_facts(entry: &mut LibraryEntry, product: &crate::product::Product) -> bool {
    let mut changed = false;

    if let Some(installable) = product.is_installable
        && entry.installable != installable
    {
        entry.installable = installable;
        changed = true;
    }

    if let Some(corrected) = match product.game_type.as_deref() {
        Some("game") | Some("pack") => Some(GameKind::Game),
        Some("dlc") => Some(GameKind::Extra),
        _ => None,
    } && entry.kind != corrected
    {
        entry.kind = corrected;
        changed = true;
    }

    changed
}

/// The one field that isn't catalog data — read from this profile's
/// appmanifest. Left unset when the game isn't installed, matching the
/// "unset, not `installed = false`" convention.
///
/// Best-effort: a library that can't be read makes every game look not
/// installed, which is wrong but harmless — the alternative is failing the
/// whole listing over it.
fn with_install_state(account_id: &str, mut entry: LibraryEntry) -> LibraryEntry {
    let installed = crate::install::library()
        .ok()
        .and_then(|library| library.read_manifest(account_id, Storefront::Gog, &entry.game.id));
    if let Some(installed) = installed {
        entry.game.install_state = Some(InstallState {
            installed: true,
            installed_version: Some(installed.build_id),
            install_size_bytes: installed.size_on_disk,
            ..Default::default()
        });
    }
    entry
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod live_rank_direction_check;
