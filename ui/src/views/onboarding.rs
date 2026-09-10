//! First-run gate: decanter shows nothing else until at least one profile
//! exists — see `main.rs::App`'s onboarding check. Reached only once, in
//! the ordinary case: creating a profile here makes it active, and the
//! check that led here never fires again with a profile already active.

use dioxus::prelude::*;

use crate::ActiveProfile;

#[component]
pub fn Onboarding() -> Element {
    let mut active_profile = use_context::<ActiveProfile>();
    let mut name = use_signal(String::new);
    let mut error = use_signal(|| None::<String>);

    let mut submit = move || {
        let name = name.read().trim().to_string();
        match crate::core::create_profile(&name) {
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
                if let Some(message) = &*error.read() {
                    p { class: "error", "{message}" }
                }
            }
        }
    }
}
