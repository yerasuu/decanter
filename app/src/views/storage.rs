//! What's taking up room, and how much is left.
//!
//! Steam's "Storage" tab, but flat rather than per-disk: decanter has one
//! library per profile, so there's one number to show for free space
//! rather than a list of drives to pick between.

use gpui::{Context, Window, div, prelude::*, px, rgb};

use crate::state::AppState;
use crate::theme;
use crate::widgets::stat_card;

pub struct StorageView;

impl StorageView {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let session = AppState::get(cx).session.clone();
        cx.observe(&session, |_, _, cx| cx.notify()).detach();
        Self
    }
}

impl Render for StorageView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let column = div().flex().flex_col().child(theme::h1("Almacenamiento"));
        let Some(profile_id) = AppState::get(cx).session.read(cx).active_profile.clone() else {
            return theme::page("storage", column.child(theme::empty("Elegi un perfil arriba.")));
        };

        // What's installed is filed per account (see `decanter_core::library`),
        // not per profile, so every account the profile groups gets its own
        // scan — a shortcut is the one exception, filed under the profile
        // itself since it was never owned by any storefront account.
        let mut games: Vec<_> = crate::core::db()
            .and_then(|db| db.accounts_in_profile(&profile_id).map_err(|err| err.to_string()))
            .inspect_err(
                |err| tracing::warn!(profile_id, error = %err, "failed to list accounts, storage view shows nothing installed"),
            )
            .unwrap_or_default()
            .into_iter()
            .flat_map(|account| crate::core::installed(&account.id).into_values())
            .collect();
        games.extend(crate::core::shortcuts(&profile_id));
        games.sort_by_key(|game| std::cmp::Reverse(game.size_on_disk.unwrap_or(0)));

        let total: u64 = games.iter().filter_map(|game| game.size_on_disk).sum();
        tracing::debug!(
            profile_id,
            game_count = games.len(),
            total_bytes = total,
            "computed storage usage"
        );
        let library_path = crate::core::library_for_profile(&profile_id)
            .inspect_err(
                |err| tracing::warn!(profile_id, error = %err, "failed to resolve library root for storage view"),
            )
            .ok()
            .map(|library| library.root().to_path_buf());
        let disk = library_path.as_deref().and_then(decanter_core::disk_usage);

        let stats = div()
            .flex()
            .flex_wrap()
            .gap(px(14.))
            .mt(px(20.))
            .mx_8()
            .child(stat_card(
                crate::format::bytes(total),
                &format!("usado por {} juego(s)", games.len()),
            ))
            .children(disk.map(|(free, total_disk)| {
                stat_card(
                    crate::format::bytes(free),
                    &format!("libres de {}", crate::format::bytes(total_disk)),
                )
            }));

        let count = games.len();
        let list = if games.is_empty() {
            theme::empty("No hay nada instalado todavia.").into_any_element()
        } else {
            theme::card()
                .children(games.into_iter().enumerate().map(|(index, game)| {
                    theme::list_row(("storage-row", index), index + 1 == count)
                        .child(div().flex_1().child(game.name))
                        .child(
                            div()
                                .text_color(rgb(theme::TEXT_MUTED))
                                .child(crate::format::bytes(game.size_on_disk.unwrap_or(0))),
                        )
                }))
                .into_any_element()
        };

        theme::page(
            "storage",
            column
                .child(stats)
                .children(library_path.map(|path| theme::muted(format!("Biblioteca: {}", path.display()))))
                .child(list),
        )
    }
}
