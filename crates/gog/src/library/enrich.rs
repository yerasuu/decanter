//! Pass two: purchase dates and GamesDB catalog data, layered onto what
//! [`super::list_games`] already returned. See the module docs on
//! [`super`] for how this fits with [`super::fetch`].

use std::collections::HashMap;

use futures_util::StreamExt;

use crate::api::client::{Image, Localized, Sessionless, parse_release_date};

use super::{Batch, GameKind, LibraryEntry};

/// How many entries the second pass resolves at a time. GamesDB answers
/// one request in well under a tenth of a second, so this is about not
/// opening a connection per game rather than about GamesDB's limits.
const ENRICH_CONCURRENCY: usize = 8;

/// How many enriched entries accumulate before being handed over. One
/// update per game would mean a re-render per game; a group means the
/// list fills in visibly without the UI doing 700 of them.
const ENRICH_BATCH: usize = 24;

/// Pass two: purchase dates and GamesDB catalog data, layered onto what
/// [`super::list_games`] already returned.
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
    let catalog = match Sessionless::new() {
        Ok(catalog) => catalog,
        Err(err) => {
            tracing::warn!(account_id, %err, "GOG HTTP client unavailable, library enrichment skipped");
            return;
        }
    };

    // Step one: GamesDB, which is per-id and has no batch endpoint, so
    // several at a time. Entries it actually indexed go out as they land;
    // the rest are held back for step two rather than being emitted
    // unchanged, which would be a cache write per game for no change.
    let total = entries.len();
    let mut resolved = futures_util::stream::iter(entries.into_iter().map(|mut entry| {
        let catalog = &catalog;
        async move {
            let indexed = match entry.game.id.parse::<i64>() {
                Ok(id) => apply_gamesdb(&mut entry.game, id, catalog).await,
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
        }
        all.push(entry);
        on_progress(all.len(), total);
    }
    if !batch.is_empty() {
        on_batch(batch);
    }

    apply_products(&mut all, &http, on_batch).await;

    // Best-effort: an undocumented endpoint going quiet should cost the
    // purchase dates, not the library. Narrower than the ranking from
    // pass one (silent on gifts and dev grants) but a real calendar date,
    // so it's kept as a fallback for whatever cycle's ranked fetch came
    // back empty.
    let purchased = match dates.await {
        Ok(Ok(purchased)) => purchased,
        Ok(Err(err)) => {
            tracing::warn!(account_id, error = %format_args!("{err:#}"), "GOG order history unavailable");
            tracing::info!(
                account_id,
                game_count = all.len(),
                "GOG library enrichment finished without purchase dates"
            );
            return;
        }
        Err(err) => {
            tracing::warn!(account_id, %err, "GOG order history task panicked");
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

/// Step two: GOG's own product API, for every entry — the category
/// correction it carries applies regardless of GamesDB coverage, and it's
/// also the only source for DLC/bundle parentage and for any field
/// (background art, most often) a GamesDB-indexed entry's own release
/// record still left blank.
///
/// Asked in batches of [`crate::product::BATCH`]. One request per entry
/// turned a few hundred products into a few hundred round trips, which on
/// a real library was the slowest thing in the whole fetch.
async fn apply_products(all: &mut [LibraryEntry], http: &reqwest::Client, on_batch: &mut impl FnMut(Batch)) {
    // Every entry: a base game GamesDB covered perfectly is still the only
    // place its DLC are listed, and its own release record can still be
    // missing a field GOG's product API has.
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
            // Metadata only fills holes GamesDB left — run unconditionally
            // rather than gating on `unindexed`, since an indexed entry can
            // still be missing a single field (e.g. no `background` on its
            // GamesDB release) that this alone can fill.
            if apply_product(&mut all[index], product) {
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

/// Layers GamesDB's richer catalog data over what GOG already said.
///
/// GOG's own data is the floor and is never cleared: an entry GamesDB has
/// never heard of keeps its title and art rather than being reduced to a
/// bare numeric id.
/// Returns whether GamesDB actually had catalog data. A record with an
/// empty `game` object is what a DLC looks like here — present, but with
/// nothing on it, which is not the same as being indexed.
async fn apply_gamesdb(game: &mut decanter_core::Game, id: i64, catalog: &Sessionless) -> bool {
    // trace, not debug, since a single enrichment pass can make hundreds of
    // these calls — at debug level they'd drown out everything else.
    let Ok(release) = catalog
        .gamesdb_release(id)
        .await
        .inspect_err(|err| tracing::trace!(id, %err, "GamesDB has no release"))
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
    // GamesDB's `icon` field is often a landscape crop rather than a real
    // square icon, so a base icon GOG's own owned-products listing already
    // set is kept in preference to it — only fills the gap when there was
    // none yet.
    game.icon_url = game.icon_url.take().or(release.icon.as_ref().map(Image::resolve));
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
    game.release_date = release.game.first_release_date.as_deref().and_then(parse_release_date);
    game.rating = release.game.aggregated_rating.map(|score| score.to_string());

    game.release_date.is_some() || !game.genres.is_empty()
}

/// Fills in from GOG's own product API what GamesDB had nothing to say
/// about, and corrects the entry's category while it's there.
///
/// Only ever adds. GOG's owned-products listing already supplied a title
/// and art for most entries, and this must not overwrite a good value
/// with a worse one — the exception is a title that is still just the
/// numeric id, which is not a title at all. Returns whether anything was
/// actually filled, so a caller isn't left re-emitting entries that gained
/// nothing.
fn apply_product(entry: &mut LibraryEntry, product: crate::product::Product) -> bool {
    let mut changed = false;
    if let Some(title) = product.title.filter(|title| !title.trim().is_empty())
        && entry.game.title == entry.game.id
    {
        entry.game.title = title;
        changed = true;
    }
    if entry.game.description.is_none()
        && let Some(lead) = product.description.lead
    {
        let text = crate::product::html_to_text(&lead);
        entry.game.description = (!text.is_empty()).then_some(text);
        changed = changed || entry.game.description.is_some();
    }
    if entry.game.release_date.is_none() {
        entry.game.release_date = product.release_date.as_deref().and_then(parse_release_date);
        changed = changed || entry.game.release_date.is_some();
    }
    if entry.game.background_url.is_none() {
        entry.game.background_url = product.images.background.as_deref().and_then(crate::product::image_url);
        changed = changed || entry.game.background_url.is_some();
    }
    if entry.game.icon_url.is_none() {
        entry.game.icon_url = product
            .images
            .icon
            .as_deref()
            .or(product.images.logo.as_deref())
            .and_then(crate::product::image_url);
        changed = changed || entry.game.icon_url.is_some();
    }
    changed
}

/// Applies the facts only GOG's product record carries — what kind of
/// product this is, whether it has anything to install, and whether a
/// macOS build of it exists — returning whether any actually changed.
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

    if let Some(has_macos_build) = product
        .content_system_compatibility
        .as_ref()
        .and_then(|systems| systems.osx)
        && entry.game.has_macos_build != Some(has_macos_build)
    {
        entry.game.has_macos_build = Some(has_macos_build);
        changed = true;
    }

    changed
}
