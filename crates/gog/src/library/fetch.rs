//! Pass one: everything GOG's own listing already knows — title, art,
//! category, and the order it considers them added. See the module docs
//! on [`super`] for how this fits with [`super::enrich`].

use std::collections::HashSet;

use anyhow::Context;
use decanter_core::{Game, InstallState, Storefront};
use serde::Deserialize;

use super::{Batch, GameKind, LibraryEntry};

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

/// Pass one: everything GOG's own listing already knows — title, art,
/// category, and the order it considers them added.
///
/// Each page is handed to `on_batch` as it lands, so the first games are
/// on screen while the rest are still being fetched. The full list comes
/// back too, for [`super::enrich_games`] to work through.
///
/// This is the pass that must not fail: it's the library itself.
pub async fn list_games(
    library: &decanter_core::Library,
    account_id: &str,
    on_batch: &mut impl FnMut(Batch),
) -> anyhow::Result<Vec<LibraryEntry>> {
    let (owned_ids, access_token) = crate::achv_try!(
        crate::with_session(account_id, |client| {
            let owned_ids = crate::achv_try!(
                client
                    .get_games()
                    .map_err(|err| anyhow::anyhow!("gog owned-games request failed: {}", crate::without_secrets(err)))
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
                with_install_state(library, account_id, entry)
            })
            .collect();
        entries.extend(batch.iter().cloned());
        on_batch(batch);
    })
    .await;

    // A failure here costs metadata and ordering, not the library — the
    // owned-id sweep below still covers everything.
    if let Err(err) = paged {
        tracing::warn!(account_id, error = %format_args!("{err:#}"), "GOG product listing failed");
    }

    // Owned but absent from the filtered view: that's what a non-game
    // grant looks like from here. They still belong in the library, just
    // without GOG-side metadata.
    let leftovers: Batch = owned_ids
        .into_iter()
        .filter(|id| !seen.contains(id))
        .map(|id| with_install_state(library, account_id, base_entry(id, None)))
        .collect();
    if !leftovers.is_empty() {
        tracing::debug!(
            leftover_count = leftovers.len(),
            "found owned products absent from GOG's filtered listing"
        );
        entries.extend(leftovers.iter().cloned());
        on_batch(leftovers);
    }

    // Already install-state-annotated above, per batch — not redone here:
    // `absorb` (app/src/state/gog_library.rs) wholesale-replaces whatever
    // it already had shown for an id with exactly what each `on_batch`
    // call carried, so an entry emitted above without this would have
    // clobbered a game's just-applied install/uninstall state until pass
    // two happened to re-emit that same entry for an unrelated reason —
    // which for an already-fully-cataloged game, it often never does.
    tracing::info!(account_id, game_count = entries.len(), "GOG library listing finished");
    Ok(entries)
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

/// The one field that isn't catalog data — read from this profile's
/// appmanifest. Left unset when the game isn't installed, matching the
/// "unset, not `installed = false`" convention.
///
/// Best-effort: a library that can't be read makes every game look not
/// installed, which is wrong but harmless — the alternative is failing the
/// whole listing over it.
fn with_install_state(library: &decanter_core::Library, account_id: &str, mut entry: LibraryEntry) -> LibraryEntry {
    let installed = library.read_manifest(account_id, Storefront::Gog, &entry.game.id);
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
