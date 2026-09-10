//! What's taking up room, and how much is left.
//!
//! Steam's "Storage" tab, but flat rather than per-disk: achv has one
//! library today (see `Library::default_path`), so there's one number to
//! show for free space rather than a list of drives to pick between.

use dioxus::prelude::*;

#[component]
pub fn Storage() -> Element {
    let active_profile = use_context::<crate::ActiveProfile>();
    let profile_id = active_profile.read().clone();

    let Some(profile_id) = profile_id else {
        return rsx! {
            div { class: "page",
                h1 { "Almacenamiento" }
                div { class: "empty", "Elegi un perfil arriba." }
            }
        };
    };

    // What's installed is filed per account (see `achv_core::library`),
    // not per profile, so every account the profile groups gets its own
    // scan — a shortcut is the one exception, filed under the profile
    // itself since it was never owned by any storefront account.
    let mut games: Vec<_> = crate::core::db()
        .and_then(|db| db.accounts_in_profile(&profile_id).map_err(|err| err.to_string()))
        .inspect_err(|err| tracing::warn!(profile_id, error = %err, "failed to list accounts, storage view shows nothing installed"))
        .unwrap_or_default()
        .into_iter()
        .flat_map(|account| crate::core::installed(&account.id).into_values().collect::<Vec<_>>())
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
    let library_path = crate::core::library()
        .inspect_err(|err| tracing::warn!(error = %err, "failed to resolve library root for storage view"))
        .ok()
        .map(|library| library.root().to_path_buf());
    let disk = library_path.as_deref().and_then(achv_core::disk_usage);

    rsx! {
        div { class: "page",
            h1 { "Almacenamiento" }

            div { class: "stat-row",
                div { class: "stat-card",
                    div { class: "num", "{crate::format::bytes(total)}" }
                    div { class: "lbl", "usado por {games.len()} juego(s)" }
                }
                if let Some((free, total_disk)) = disk {
                    div { class: "stat-card",
                        div { class: "num", "{crate::format::bytes(free)}" }
                        div { class: "lbl", "libres de {crate::format::bytes(total_disk)}" }
                    }
                }
            }

            if let Some(path) = &library_path {
                p { class: "muted", "Biblioteca: {path.display()}" }
            }

            if games.is_empty() {
                div { class: "empty", "No hay nada instalado todavia." }
            } else {
                div { class: "card",
                    for game in games {
                        div { class: "list-row", key: "{game.game_id}",
                            span { style: "flex: 1;", "{game.name}" }
                            span { class: "muted", "{crate::format::bytes(game.size_on_disk.unwrap_or(0))}" }
                        }
                    }
                }
            }
        }
    }
}
