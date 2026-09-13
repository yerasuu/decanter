//! The GOG library, fetched by us directly rather than through an
//! external plugin registry chain that would just reach the same public
//! endpoints this module calls itself — decanter owns the whole path, and
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
//! a minute. [`fetch::list_games`] returns what GOG's own listing already
//! knows — title, art, category — a page at a time, so the library is on
//! screen and in the cache almost immediately. [`enrich::enrich_games`]
//! then layers GamesDB and the order history on top, many at a time,
//! updating entries in place. Closing the app between the two costs the
//! metadata, not the library.

use anyhow::Context;
use decanter_core::Game;

mod enrich;
mod fetch;

pub use enrich::enrich_games;
pub use fetch::list_games;

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

/// A group of entries ready to show. Emitted as they resolve rather than
/// returned all at once — see the module docs.
pub type Batch = Vec<LibraryEntry>;

pub async fn find_game(account_id: &str, product_id: &str) -> anyhow::Result<Game> {
    // Ignored: this only needs the finished list, not the progress.
    crate::achv_try!(list_games(account_id, &mut |_| {}).await)
        .into_iter()
        .map(|entry| entry.game)
        .find(|game| game.id == product_id)
        .with_context(|| format!("product {product_id} not found in profile {account_id}'s library"))
}
