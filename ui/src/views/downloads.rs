//! Every install in flight, in one place — the same `installing` map the
//! per-game action bar reads, listed instead of scattered across game
//! pages so a download can be watched or cancelled without hunting for
//! the game that started it.

use dioxus::prelude::*;

use crate::background::{Background, InstallStatus};
use crate::gog_library::GogLibrary;
use crate::{format, views::library::install};

#[component]
pub fn Downloads() -> Element {
    let Background { installing, .. } = use_context::<Background>();
    let library = use_context::<GogLibrary>();
    let active_profile = use_context::<crate::ActiveProfile>();

    // Sorted by title so the list doesn't reshuffle on every progress
    // tick — a `HashMap`'s iteration order is arbitrary and unstable.
    let mut entries: Vec<(String, String, InstallStatus)> = installing
        .read()
        .iter()
        .map(|(product_id, status)| {
            let title = library
                .games
                .read()
                .iter()
                .find(|entry| &entry.game.id == product_id)
                .map_or_else(|| product_id.clone(), |entry| entry.game.title.clone());
            (product_id.clone(), title, status.clone())
        })
        .collect();
    entries.sort_by(|left, right| left.1.cmp(&right.1));

    rsx! {
        div { class: "page",
            h1 { "Descargas" }

            if entries.is_empty() {
                div { class: "empty", "No hay descargas en curso." }
            } else {
                div { class: "card",
                    for (product_id, title, status) in entries {
                        DownloadRow {
                            key: "{product_id}",
                            product_id,
                            title,
                            status,
                            profile_id: active_profile.read().clone().unwrap_or_default(),
                            installing,
                            library,
                        }
                    }
                }
            }
        }
    }
}

#[component]
fn DownloadRow(
    product_id: String,
    title: String,
    status: InstallStatus,
    profile_id: String,
    installing: Signal<std::collections::HashMap<String, InstallStatus>>,
    library: GogLibrary,
) -> Element {
    let mut installing = installing;

    rsx! {
        div { class: "list-row download-row",
            div { class: "download-main",
                div { class: "download-title", "{title}" }

                match &status {
                    InstallStatus::Queued => rsx! {
                        div { class: "bar-track indeterminate", div { class: "bar-fill", style: "width: 100%" } }
                        div { class: "muted install-progress-label", "En cola…" }
                    },
                    InstallStatus::Installing { current_file, bytes_downloaded, total_bytes, bytes_per_second } => {
                        let fraction = status.fraction();
                        let width = fraction.map_or_else(|| "100%".to_string(), |value| format!("{:.1}%", value * 100.0));
                        let remaining = total_bytes.unwrap_or(0).saturating_sub(*bytes_downloaded);
                        rsx! {
                            div {
                                class: if fraction.is_some() { "bar-track" } else { "bar-track indeterminate" },
                                div { class: "bar-fill", style: "width: {width}" }
                            }
                            div { class: "muted install-progress-label",
                                "{format::bytes(*bytes_downloaded)}"
                                if let Some(total) = total_bytes {
                                    " / {format::bytes(*total)}"
                                }
                                " · {format::rate(*bytes_per_second)}"
                                if let Some(left) = format::eta(remaining, *bytes_per_second) {
                                    " · faltan {left}"
                                }
                            }
                            div { class: "muted download-file", "{current_file}" }
                        }
                    },
                    InstallStatus::Failed(err) => rsx! {
                        div { class: "error", "{err}" }
                    },
                }
            }

            div { class: "download-actions",
                match &status {
                    // A failure is already over; the row is just a notice
                    // the user hasn't read yet, so it's dismissed rather
                    // than cancelled.
                    InstallStatus::Failed(_) => {
                        let product_id = product_id;
                        rsx! {
                            button {
                                class: "icon-btn",
                                onclick: move |_| { installing.write().remove(&product_id); },
                                "Descartar"
                            }
                        }
                    }
                    _ => {
                        let profile_id = profile_id;
                        let product_id = product_id;
                        rsx! {
                            button {
                                class: "icon-btn",
                                onclick: move |_| {
                                    let profile_id = profile_id.clone();
                                    let product_id = product_id.clone();
                                    spawn(async move {
                                        install::cancel(profile_id.clone(), product_id.clone()).await;
                                        // Cancelling only stops the transfer;
                                        // nothing else clears the row.
                                        installing.write().remove(&product_id);
                                        library.refresh_install_state(&profile_id, &product_id);
                                    });
                                },
                                "Cancelar"
                            }
                        }
                    }
                }
            }
        }
    }
}
