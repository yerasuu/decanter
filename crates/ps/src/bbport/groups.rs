//! The settings view's table: every setting decanter edits, where it is
//! stored, and the values bbport assumes when it is absent.

/// Where a setting is kept: `bbport.ini` (read by bbport itself and its in-game menu) or the
/// launch settings (turned into environment variables at launch).
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Store {
    Ini,
    Launcher,
}

pub enum Control {
    /// `(label, value, note shown while selected)`; `default` is the value bbport assumes when the
    /// key is absent.
    Choice {
        options: &'static [(&'static str, &'static str, Option<&'static str>)],
        default: &'static str,
    },
    Toggle {
        default: bool,
    },
    /// Stepped with ‹ ›; `decimals` is how the ini writes it (`sharpness=0.50`).
    Number {
        min: f64,
        max: f64,
        step: f64,
        default: f64,
        decimals: usize,
    },
    /// The controller list from `out/bb-gpu-capabilities --gamepads` (key `gamepad`, a GUID).
    Gamepad,
}

pub struct Setting {
    pub key: &'static str,
    pub label: &'static str,
    pub note: Option<&'static str>,
    pub store: Store,
    pub control: Control,
}

pub struct Group {
    pub title: &'static str,
    pub note: Option<&'static str>,
    pub settings: &'static [Setting],
}

const fn toggle(
    key: &'static str,
    label: &'static str,
    note: Option<&'static str>,
    store: Store,
    default: bool,
) -> Setting {
    Setting {
        key,
        label,
        note,
        store,
        control: Control::Toggle { default },
    }
}

/// The settings page, grouped and valued as in bbport 0.4's launcher
/// (`launcher/bbport_launcher.py`: `build_settings_page`, `DEFAULTS`, `INI_DEFAULTS`).
/// Mods, external patches, the FSR 4.1.1 build and the free-form environment have views of
/// their own.
pub const GROUPS: &[Group] = &[
    Group {
        title: "Juego",
        note: None,
        settings: &[
            Setting {
                key: "language",
                label: "Idioma del juego",
                note: None,
                store: Store::Launcher,
                control: Control::Choice {
                    options: &[
                        ("Inglés", "1", None),
                        ("Español", "3", None),
                        ("Francés", "2", None),
                        ("Alemán", "4", None),
                        ("Italiano", "5", None),
                        ("Ruso", "8", None),
                        ("Japonés", "0", None),
                    ],
                    default: "1",
                },
            },
            toggle(
                "mods_enabled",
                "Cargar mods",
                Some("La lista y el orden de los mods se gestionan en la pestaña Mods"),
                Store::Launcher,
                true,
            ),
        ],
    },
    Group {
        title: "Modo de funcionamiento",
        note: None,
        settings: &[toggle(
            "pc_model",
            "Nuevo modelo de memoria y traducción",
            Some(
                "Experimental, solo GPU AMD: la GPU usa la memoria del juego directamente y los comandos \
                 gráficos se traducen en vez de emularse. Apagado: el modelo de la 0.3 con sus arreglos",
            ),
            Store::Launcher,
            false,
        )],
    },
    Group {
        title: "Pantalla",
        note: None,
        settings: &[
            Setting {
                key: "output_res",
                label: "Resolución de salida",
                note: Some("El reescalado completa el fotograma; en Steam Deck, 720p"),
                store: Store::Ini,
                control: Control::Choice {
                    options: &[
                        ("1280×720 (Steam Deck)", "1280x720", None),
                        ("1920×1080", "1920x1080", None),
                        ("2560×1440", "2560x1440", None),
                        ("3840×2160", "3840x2160", None),
                    ],
                    default: "1920x1080",
                },
            },
            Setting {
                key: "live_resolution",
                label: "Cambio de resolución en caliente",
                note: Some("Sin reiniciar, pero más lento en Steam Deck y GPU antiguas"),
                store: Store::Ini,
                control: Control::Choice {
                    options: &[
                        ("Automático (según la GPU)", "auto", None),
                        ("Desactivado (más rápido)", "0", None),
                        ("Activado", "1", None),
                    ],
                    default: "0",
                },
            },
            toggle("fullscreen", "Pantalla completa", None, Store::Launcher, false),
            Setting {
                key: "present_mode",
                label: "Modo de presentación",
                note: None,
                store: Store::Launcher,
                control: Control::Choice {
                    options: &[
                        ("Mailbox", "Mailbox", None),
                        ("FIFO (VSync)", "Fifo", None),
                        ("FIFO Relaxed", "FifoRelaxed", None),
                        ("Immediate", "Immediate", None),
                    ],
                    default: "Mailbox",
                },
            },
            toggle("hdr", "Permitir HDR", None, Store::Launcher, false),
        ],
    },
    Group {
        title: "Mando",
        note: None,
        settings: &[Setting {
            key: "gamepad",
            label: "Mando",
            note: Some("Con varios conectados, cuál usa el juego; la lista se actualiza al abrir esta pestaña"),
            store: Store::Launcher,
            control: Control::Gamepad,
        }],
    },
    Group {
        title: "Reescalado",
        note: Some("Se guarda en bbport.ini; en el juego se cambia desde el menú (Insert o L3+R3)"),
        settings: &[
            Setting {
                key: "upscaler",
                label: "Reescalado",
                note: None,
                store: Store::Ini,
                control: Control::Choice {
                    options: &[
                        ("FSR 4", "fsr4", None),
                        ("FSR 4.1.1", "fsr411", None),
                        ("FSR 3", "fsr3", None),
                        (
                            "TAA",
                            "taa",
                            Some("Antialiasing a la resolución de salida, sin modelo FSR"),
                        ),
                        ("Desactivado", "off", None),
                    ],
                    default: "fsr4",
                },
            },
            Setting {
                key: "preset",
                label: "Preset",
                note: None,
                store: Store::Ini,
                control: Control::Choice {
                    options: &[
                        ("Native AA", "0", None),
                        ("Quality (x1.5)", "1", None),
                        ("Balanced (x1.7)", "2", None),
                        ("Performance (x2)", "3", None),
                        ("Ultra Performance (x3)", "4", None),
                    ],
                    default: "4",
                },
            },
            toggle("sharpen", "Nitidez (RCAS)", None, Store::Ini, true),
            Setting {
                key: "sharpness",
                label: "Intensidad de la nitidez",
                note: None,
                store: Store::Ini,
                control: Control::Number {
                    min: 0.0,
                    max: 2.0,
                    step: 0.05,
                    default: 0.5,
                    decimals: 2,
                },
            },
            toggle(
                "object_motion",
                "Vectores de movimiento de objetos",
                Some("Menos estelas en los personajes; cuesta un 10% de FPS"),
                Store::Ini,
                true,
            ),
            toggle("show_fps", "Mostrar FPS", None, Store::Ini, true),
        ],
    },
    Group {
        title: "Efectos del juego",
        note: Some("Parches del juego, se aplican al arrancar"),
        settings: &[
            Setting {
                key: "model_lod",
                label: "Detalle de modelos",
                note: None,
                store: Store::Ini,
                control: Control::Choice {
                    options: &[
                        ("Como en el juego", "0", None),
                        ("Máximo (-2)", "-2", None),
                        ("Menor (1)", "1", None),
                        ("Mínimo (2)", "2", None),
                    ],
                    default: "0",
                },
            },
            toggle(
                "effect_chromatic_aberration",
                "Aberración cromática",
                None,
                Store::Ini,
                true,
            ),
            toggle("effect_dof", "Profundidad de campo", None, Store::Ini, true),
            toggle("effect_motion_blur", "Desenfoque de movimiento", None, Store::Ini, true),
            toggle("effect_ssao", "SSAO", None, Store::Ini, true),
            toggle("effect_game_aa", "Antialiasing del juego", None, Store::Ini, true),
            toggle(
                "effect_dynamic_shadows",
                "Sombras de luces dinámicas",
                None,
                Store::Ini,
                true,
            ),
            toggle(
                "effect_ssr",
                "Reflejos SSR (no estaban en el juego)",
                None,
                Store::Ini,
                false,
            ),
            toggle("skip_intro", "Saltar vídeos de inicio", None, Store::Ini, false),
            toggle(
                "debug_camera",
                "Cámara libre (Cruz + L3 / Space + Z)",
                None,
                Store::Ini,
                false,
            ),
            toggle(
                "debug_menu",
                "Menú de depuración (táctil izquierdo / Tab)",
                Some("Necesita DbgFont14h.ccm y DbgFont14h.tpf en dvdroot_ps4/font del juego"),
                Store::Ini,
                false,
            ),
        ],
    },
    Group {
        title: "Fotogramas",
        note: None,
        settings: &[
            Setting {
                key: "fps_mode",
                label: "Modo",
                note: Some("Qué parche de fotogramas se aplica al juego"),
                store: Store::Launcher,
                control: Control::Choice {
                    options: &[
                        ("Sin límite (parche)", "uncap", None),
                        ("60", "60", None),
                        ("90", "90", None),
                        ("30 (como en PS4)", "30", None),
                    ],
                    default: "uncap",
                },
            },
            Setting {
                key: "fps_limit",
                label: "Límite de FPS",
                note: Some("0: sin límite"),
                store: Store::Launcher,
                control: Control::Number {
                    min: 0.0,
                    max: 480.0,
                    step: 10.0,
                    default: 0.0,
                    decimals: 0,
                },
            },
        ],
    },
    Group {
        title: "Rendimiento",
        note: None,
        settings: &[
            Setting {
                key: "draw_pipe",
                label: "Pipeline de GPU en dos fases",
                note: Some("Un 20–30% más rápido; desactívalo si hay inestabilidad"),
                store: Store::Launcher,
                control: Control::Choice {
                    options: &[
                        ("Automático", "", Some("Activado con 8 o más hilos de CPU")),
                        ("Activado", "1", None),
                        ("Desactivado", "0", Some("Más estable, pero más lento")),
                    ],
                    default: "",
                },
            },
            Setting {
                key: "readbacks",
                label: "Lecturas de datos de la GPU",
                note: None,
                store: Store::Launcher,
                control: Control::Choice {
                    options: &[
                        ("Relaxed", "", Some("Por defecto")),
                        ("Desactivadas", "0", None),
                        ("Precise", "2", None),
                    ],
                    default: "",
                },
            },
            Setting {
                key: "preupload",
                label: "Carga previa a la memoria de vídeo",
                note: Some("Menos tirones al cargar zonas"),
                store: Store::Launcher,
                control: Control::Choice {
                    options: &[
                        ("Normal", "", Some("Sin memoria de vídeo extra")),
                        ("Completa", "2", Some("Unos 3 GB más de memoria de vídeo")),
                        ("Desactivada", "0", None),
                    ],
                    default: "",
                },
            },
        ],
    },
    Group {
        title: "Desarrollo",
        note: None,
        settings: &[
            toggle("mangohud", "MangoHud", None, Store::Launcher, false),
            toggle(
                "save_log",
                "Guardar registro y estadísticas en archivo",
                Some("En la carpeta logs de bbport: para analizar tirones y cierres"),
                Store::Launcher,
                false,
            ),
            toggle(
                "crash_diag",
                "Diagnóstico de cierres",
                Some("Comprueba el heap del juego y registra escrituras en su memoria; algo más lento"),
                Store::Launcher,
                false,
            ),
            toggle(
                "as_0_3",
                "Sincronización como en la 0.3",
                Some("Para buscar regresiones: modelo de memoria y esperas de copia como en la 0.3"),
                Store::Launcher,
                false,
            ),
            toggle(
                "frame_stats",
                "Estadísticas de fotogramas en el registro",
                Some("BB_FRAME_STATS"),
                Store::Launcher,
                false,
            ),
            toggle(
                "gpu_profile",
                "Perfil de GPU en el registro",
                Some("BB_GPU_PROFILE"),
                Store::Launcher,
                false,
            ),
            toggle(
                "vk_validation",
                "Capas de validación de Vulkan",
                Some("Ralentiza mucho"),
                Store::Launcher,
                false,
            ),
        ],
    },
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_table_is_consistent() {
        let mut keys = std::collections::HashSet::new();
        for setting in GROUPS.iter().flat_map(|group| group.settings) {
            assert!(keys.insert(setting.key), "{} listed twice", setting.key);
            match &setting.control {
                Control::Choice { options, default } => {
                    assert!(
                        options.iter().any(|o| o.1 == *default),
                        "{}: default not an option",
                        setting.key
                    )
                }
                Control::Number { min, max, default, .. } => {
                    assert!((*min..=*max).contains(default), "{}", setting.key)
                }
                Control::Toggle { .. } | Control::Gamepad => {}
            }
        }
    }
}
