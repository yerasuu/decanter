//! The settings view's table: every setting decanter edits, where it is
//! stored, and the values bbport assumes when it is absent. Titles, labels
//! and notes are the app's locale keys.

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
    /// The controller list from the port's GPU tool, `--gamepads` (key `gamepad`, a GUID).
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
/// (`launcher/psport_launcher.py`: `build_settings_page`, `DEFAULTS`, `INI_DEFAULTS`).
/// Mods, external patches, the FSR 4.1.1 build and the free-form environment have views of
/// their own.
pub const GROUPS: &[Group] = &[
    Group {
        title: "ps.graphics.groups.game",
        note: None,
        settings: &[
            Setting {
                key: "language",
                label: "ps.graphics.language.label",
                note: None,
                store: Store::Launcher,
                control: Control::Choice {
                    options: &[
                        ("ps.graphics.language.options.english", "1", None),
                        ("ps.graphics.language.options.spanish", "3", None),
                        ("ps.graphics.language.options.french", "2", None),
                        ("ps.graphics.language.options.german", "4", None),
                        ("ps.graphics.language.options.italian", "5", None),
                        ("ps.graphics.language.options.russian", "8", None),
                        ("ps.graphics.language.options.japanese", "0", None),
                    ],
                    default: "1",
                },
            },
            toggle(
                "mods_enabled",
                "ps.graphics.mods_enabled.label",
                Some("ps.graphics.mods_enabled.note"),
                Store::Launcher,
                true,
            ),
        ],
    },
    Group {
        title: "ps.graphics.groups.mode",
        note: None,
        settings: &[toggle(
            "pc_model",
            "ps.graphics.pc_model.label",
            Some("ps.graphics.pc_model.note"),
            Store::Launcher,
            false,
        )],
    },
    Group {
        title: "ps.graphics.groups.display",
        note: None,
        settings: &[
            Setting {
                key: "output_res",
                label: "ps.graphics.output_res.label",
                note: Some("ps.graphics.output_res.note"),
                store: Store::Ini,
                control: Control::Choice {
                    options: &[
                        ("ps.graphics.output_res.options.r720", "1280x720", None),
                        ("ps.graphics.output_res.options.r1080", "1920x1080", None),
                        ("ps.graphics.output_res.options.r1440", "2560x1440", None),
                        ("ps.graphics.output_res.options.r2160", "3840x2160", None),
                    ],
                    default: "1920x1080",
                },
            },
            Setting {
                key: "aspect_ratio",
                label: "ps.graphics.aspect_ratio.label",
                note: Some("ps.graphics.aspect_ratio.note"),
                store: Store::Ini,
                control: Control::Choice {
                    options: &[
                        ("ps.graphics.aspect_ratio.options.auto", "auto", None),
                        ("ps.graphics.aspect_ratio.options.r4_3", "4:3", None),
                        ("ps.graphics.aspect_ratio.options.r16_10", "16:10", None),
                        ("ps.graphics.aspect_ratio.options.r16_9", "16:9", None),
                        ("ps.graphics.aspect_ratio.options.r21_9", "21:9", None),
                        ("ps.graphics.aspect_ratio.options.r32_9", "32:9", None),
                    ],
                    // The port assumes 16:9 when absent, but decanter then passes the display's
                    // ratio ([`super::nearest_aspect_ratio`]), as it does for `auto`.
                    default: "auto",
                },
            },
            Setting {
                key: "live_resolution",
                label: "ps.graphics.live_resolution.label",
                note: Some("ps.graphics.live_resolution.note"),
                store: Store::Ini,
                control: Control::Choice {
                    options: &[
                        ("ps.graphics.live_resolution.options.auto", "auto", None),
                        ("ps.graphics.live_resolution.options.off", "0", None),
                        ("ps.graphics.live_resolution.options.on", "1", None),
                    ],
                    default: "0",
                },
            },
            toggle(
                "fullscreen",
                "ps.graphics.fullscreen.label",
                None,
                Store::Launcher,
                false,
            ),
            Setting {
                key: "present_mode",
                label: "ps.graphics.present_mode.label",
                note: None,
                store: Store::Launcher,
                control: Control::Choice {
                    options: &[
                        ("ps.graphics.present_mode.options.mailbox", "Mailbox", None),
                        ("ps.graphics.present_mode.options.fifo", "Fifo", None),
                        ("ps.graphics.present_mode.options.fifo_relaxed", "FifoRelaxed", None),
                        ("ps.graphics.present_mode.options.immediate", "Immediate", None),
                    ],
                    default: "Mailbox",
                },
            },
            toggle("hdr", "ps.graphics.hdr.label", None, Store::Launcher, false),
        ],
    },
    Group {
        title: "ps.graphics.groups.gamepad",
        note: None,
        settings: &[Setting {
            key: "gamepad",
            label: "ps.graphics.gamepad.label",
            note: Some("ps.graphics.gamepad.note"),
            store: Store::Launcher,
            control: Control::Gamepad,
        }],
    },
    Group {
        title: "ps.graphics.groups.mouse",
        note: Some("ps.graphics.groups.mouse_note"),
        settings: &[
            Setting {
                key: "game_mouse_sensitivity",
                label: "ps.graphics.game_mouse_sensitivity.label",
                note: Some("ps.graphics.game_mouse_sensitivity.note"),
                store: Store::Ini,
                control: Control::Number {
                    min: 0.1,
                    max: 5.0,
                    step: 0.1,
                    default: 1.0,
                    decimals: 2,
                },
            },
            toggle(
                "mouse_look",
                "ps.graphics.mouse_look.label",
                Some("ps.graphics.mouse_look.note"),
                Store::Ini,
                true,
            ),
            Setting {
                key: "mouse_sensitivity",
                label: "ps.graphics.mouse_sensitivity.label",
                note: Some("ps.graphics.mouse_sensitivity.note"),
                store: Store::Ini,
                control: Control::Number {
                    min: 0.1,
                    max: 5.0,
                    step: 0.1,
                    default: 1.0,
                    decimals: 2,
                },
            },
            toggle(
                "mouse_invert_y",
                "ps.graphics.mouse_invert_y.label",
                None,
                Store::Ini,
                false,
            ),
        ],
    },
    Group {
        title: "ps.graphics.groups.upscaling",
        note: Some("ps.graphics.groups.upscaling_note"),
        settings: &[
            Setting {
                key: "upscaler",
                label: "ps.graphics.upscaler.label",
                note: None,
                store: Store::Ini,
                control: Control::Choice {
                    options: &[
                        ("ps.graphics.upscaler.options.fsr4", "fsr4", None),
                        ("ps.graphics.upscaler.options.fsr411", "fsr411", None),
                        ("ps.graphics.upscaler.options.fsr3", "fsr3", None),
                        (
                            "ps.graphics.upscaler.options.taa",
                            "taa",
                            Some("ps.graphics.upscaler.options.taa_note"),
                        ),
                        ("ps.graphics.upscaler.options.off", "off", None),
                    ],
                    default: "fsr4",
                },
            },
            Setting {
                key: "preset",
                label: "ps.graphics.preset.label",
                note: None,
                store: Store::Ini,
                control: Control::Choice {
                    options: &[
                        ("ps.graphics.preset.options.native", "0", None),
                        ("ps.graphics.preset.options.quality", "1", None),
                        ("ps.graphics.preset.options.balanced", "2", None),
                        ("ps.graphics.preset.options.performance", "3", None),
                        ("ps.graphics.preset.options.ultra_performance", "4", None),
                    ],
                    default: "4",
                },
            },
            toggle("sharpen", "ps.graphics.sharpen.label", None, Store::Ini, true),
            Setting {
                key: "sharpness",
                label: "ps.graphics.sharpness.label",
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
                "ps.graphics.object_motion.label",
                Some("ps.graphics.object_motion.note"),
                Store::Ini,
                true,
            ),
            toggle("show_fps", "ps.graphics.show_fps.label", None, Store::Ini, true),
        ],
    },
    Group {
        title: "ps.graphics.groups.effects",
        note: Some("ps.graphics.groups.effects_note"),
        settings: &[
            Setting {
                key: "model_lod",
                label: "ps.graphics.model_lod.label",
                note: None,
                store: Store::Ini,
                control: Control::Choice {
                    options: &[
                        ("ps.graphics.model_lod.options.game", "0", None),
                        ("ps.graphics.model_lod.options.max", "-2", None),
                        ("ps.graphics.model_lod.options.lower", "1", None),
                        ("ps.graphics.model_lod.options.min", "2", None),
                    ],
                    default: "0",
                },
            },
            toggle(
                "effect_chromatic_aberration",
                "ps.graphics.effect_chromatic_aberration.label",
                None,
                Store::Ini,
                true,
            ),
            toggle("effect_dof", "ps.graphics.effect_dof.label", None, Store::Ini, true),
            toggle(
                "effect_motion_blur",
                "ps.graphics.effect_motion_blur.label",
                None,
                Store::Ini,
                true,
            ),
            toggle("effect_ssao", "ps.graphics.effect_ssao.label", None, Store::Ini, true),
            toggle(
                "effect_game_aa",
                "ps.graphics.effect_game_aa.label",
                None,
                Store::Ini,
                true,
            ),
            toggle(
                "effect_dynamic_shadows",
                "ps.graphics.effect_dynamic_shadows.label",
                None,
                Store::Ini,
                true,
            ),
            toggle("effect_ssr", "ps.graphics.effect_ssr.label", None, Store::Ini, false),
            toggle("skip_intro", "ps.graphics.skip_intro.label", None, Store::Ini, false),
            toggle(
                "debug_camera",
                "ps.graphics.debug_camera.label",
                None,
                Store::Ini,
                false,
            ),
            toggle(
                "debug_menu",
                "ps.graphics.debug_menu.label",
                Some("ps.graphics.debug_menu.note"),
                Store::Ini,
                false,
            ),
        ],
    },
    Group {
        title: "ps.graphics.groups.frames",
        note: None,
        settings: &[
            Setting {
                key: "fps_mode",
                label: "ps.graphics.fps_mode.label",
                note: Some("ps.graphics.fps_mode.note"),
                store: Store::Launcher,
                control: Control::Choice {
                    options: &[
                        ("ps.graphics.fps_mode.options.uncap", "uncap", None),
                        ("ps.graphics.fps_mode.options.fps60", "60", None),
                        ("ps.graphics.fps_mode.options.fps90", "90", None),
                        ("ps.graphics.fps_mode.options.fps30", "30", None),
                    ],
                    default: "uncap",
                },
            },
            Setting {
                key: "fps_limit",
                label: "ps.graphics.fps_limit.label",
                note: Some("ps.graphics.fps_limit.note"),
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
        title: "ps.graphics.groups.performance",
        note: None,
        settings: &[
            Setting {
                key: "draw_pipe",
                label: "ps.graphics.draw_pipe.label",
                note: Some("ps.graphics.draw_pipe.note"),
                store: Store::Launcher,
                control: Control::Choice {
                    options: &[
                        (
                            "ps.graphics.draw_pipe.options.auto",
                            "",
                            Some("ps.graphics.draw_pipe.options.auto_note"),
                        ),
                        ("ps.graphics.draw_pipe.options.on", "1", None),
                        (
                            "ps.graphics.draw_pipe.options.off",
                            "0",
                            Some("ps.graphics.draw_pipe.options.off_note"),
                        ),
                    ],
                    default: "",
                },
            },
            Setting {
                key: "readbacks",
                label: "ps.graphics.readbacks.label",
                note: None,
                store: Store::Launcher,
                control: Control::Choice {
                    options: &[
                        (
                            "ps.graphics.readbacks.options.relaxed",
                            "",
                            Some("ps.graphics.readbacks.options.relaxed_note"),
                        ),
                        ("ps.graphics.readbacks.options.off", "0", None),
                        ("ps.graphics.readbacks.options.precise", "2", None),
                    ],
                    default: "",
                },
            },
            Setting {
                key: "preupload",
                label: "ps.graphics.preupload.label",
                note: Some("ps.graphics.preupload.note"),
                store: Store::Launcher,
                control: Control::Choice {
                    options: &[
                        (
                            "ps.graphics.preupload.options.normal",
                            "",
                            Some("ps.graphics.preupload.options.normal_note"),
                        ),
                        (
                            "ps.graphics.preupload.options.full",
                            "2",
                            Some("ps.graphics.preupload.options.full_note"),
                        ),
                        ("ps.graphics.preupload.options.off", "0", None),
                    ],
                    default: "",
                },
            },
        ],
    },
    Group {
        title: "ps.graphics.groups.development",
        note: None,
        settings: &[
            toggle(
                "save_log",
                "ps.graphics.save_log.label",
                Some("ps.graphics.save_log.note"),
                Store::Launcher,
                false,
            ),
            toggle(
                "crash_diag",
                "ps.graphics.crash_diag.label",
                Some("ps.graphics.crash_diag.note"),
                Store::Launcher,
                false,
            ),
            toggle(
                "as_0_3",
                "ps.graphics.as_0_3.label",
                Some("ps.graphics.as_0_3.note"),
                Store::Launcher,
                false,
            ),
            toggle(
                "frame_stats",
                "ps.graphics.frame_stats.label",
                Some("ps.graphics.frame_stats.note"),
                Store::Launcher,
                false,
            ),
            toggle(
                "gpu_profile",
                "ps.graphics.gpu_profile.label",
                Some("ps.graphics.gpu_profile.note"),
                Store::Launcher,
                false,
            ),
            toggle(
                "vk_validation",
                "ps.graphics.vk_validation.label",
                Some("ps.graphics.vk_validation.note"),
                Store::Launcher,
                false,
            ),
        ],
    },
];
