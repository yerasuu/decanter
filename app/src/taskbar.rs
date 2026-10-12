//! How the desktop tells one game launched from decanter from another.

use decanter_core::{InstalledGame, Storefront};

/// A PC game's [`decanter_core::game_key`], prefixed by where it came from
/// so a GOG product id and an external game's uuid never meet.
pub fn key(game: &InstalledGame) -> String {
    let source = if game.storefront == Storefront::Gog {
        "gog"
    } else {
        "ext"
    };
    decanter_core::game_key(&format!("{source}_{}", game.game_id))
}
