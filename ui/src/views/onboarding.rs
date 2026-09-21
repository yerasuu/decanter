//! First-run gate: decanter shows nothing else until at least one profile
//! exists — see `main.rs::App`'s onboarding check. Reached only once, in
//! the ordinary case: creating a profile here makes it active, and the
//! check that led here never fires again with a profile already active.

use dioxus::prelude::*;

use crate::ActiveProfile;

/// The pre-filled default shown before the user ever touches the folder
/// picker — same fallback `decanter_core::Library::default_path()` itself
/// resolves to, just rendered as a string here.
fn default_library_path_display() -> String {
    decanter_core::Library::default_path().map_or_else(|| String::new(), |path| path.display().to_string())
}

#[component]
pub fn Onboarding() -> Element {
    let mut active_profile = use_context::<ActiveProfile>();
    let mut name = use_signal(String::new);
    let mut library_path = use_signal(default_library_path_display);
    // Only `Some` once the user actually picks a folder — leaving the
    // pre-filled default alone must create the profile with no
    // `library_path` set, same as before this feature existed.
    let mut library_path_chosen = use_signal(|| false);
    let mut error = use_signal(|| None::<String>);

    let mut submit = move || {
        let name = name.read().trim().to_string();
        let chosen_path = library_path_chosen().then(|| library_path.read().clone());
        match crate::core::create_profile(&name, chosen_path.as_deref()) {
            Ok(profile) => {
                tracing::info!(profile_id = %profile.id, name, "profile created");
                active_profile.set(Some(profile.id));
            }
            Err(err) => {
                tracing::error!(name, error = %err, "failed to create profile");
                error.set(Some(err));
            }
        }
    };

    rsx! {
        div { class: "onboarding",
            div { class: "onboarding-inner",
                h1 { "Bienvenido a Decanter" }
                p { class: "muted", "Creá un perfil para empezar — después podés sumarle cuentas de GOG (y de otras tiendas más adelante) desde Ajustes." }
                div { class: "field-row",
                    input {
                        r#type: "text",
                        placeholder: "Nombre del perfil",
                        value: "{name}",
                        autofocus: true,
                        oninput: move |event| name.set(event.value()),
                        onkeydown: move |event: KeyboardEvent| {
                            if event.key() == Key::Enter && !name.read().trim().is_empty() {
                                submit();
                            }
                        },
                    }
                    button {
                        disabled: name.read().trim().is_empty(),
                        onclick: move |_| submit(),
                        "Crear perfil"
                    }
                }
                p { class: "muted", "Dónde vive la biblioteca de este perfil (instalaciones, prefijos de Proton):" }
                div { class: "field-row",
                    input { r#type: "text", readonly: true, value: "{library_path}" }
                    button {
                        class: "secondary",
                        onclick: move |_| {
                            spawn(async move {
                                if let Some(folder) = rfd::AsyncFileDialog::new()
                                    .set_title("Elegí dónde vive la biblioteca de este perfil")
                                    .pick_folder()
                                    .await
                                {
                                    // Content goes under `.../steamapps`,
                                    // matching Steam's own layout for a
                                    // library folder — see
                                    // `Library::root_under`'s own doc.
                                    let root = decanter_core::Library::root_under(folder.path());
                                    tracing::debug!(path = %root.display(), "profile library path picked from file dialog");
                                    library_path.set(root.to_string_lossy().into_owned());
                                    library_path_chosen.set(true);
                                }
                            });
                        },
                        "Elegir carpeta"
                    }
                }
                if let Some(message) = &*error.read() {
                    p { class: "error", "{message}" }
                }
            }
        }
    }
}
