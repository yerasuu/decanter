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

    let mut games: Vec<_> = crate::core::installed(&profile_id).into_values().collect();
    games.extend(crate::core::shortcuts(&profile_id));
    games.sort_by_key(|game| std::cmp::Reverse(game.size_on_disk.unwrap_or(0)));

    let total: u64 = games.iter().filter_map(|game| game.size_on_disk).sum();
    let library_path = crate::core::library().ok().map(|library| library.root().to_path_buf());
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
