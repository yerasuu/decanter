//! Gamepad layouts for bbport: which physical button reaches which PS4
//! input, as the `pad.<input>=<SDL button>` lines of `bbport.ini`.
//!
//! A layout stores only the inputs it moves away from bbport's built-in
//! mapping, so the default layout writes no line at all and follows
//! upstream if that mapping ever changes. Applying one replaces every
//! `pad.` line, so switching never leaves a stray binding behind.

use std::io;

use serde::{Deserialize, Serialize};

use super::{Bbport, Ini};
use crate::paths::Root;

const PREFIX: &str = "pad.";

/// `(input, its default SDL buttons, PS4 label, what Bloodborne does with
/// it)`, the last two as the app's locale keys. Defaults as `bind_defaults`
/// in bbport's `src/runtime_pad.c`.
pub const INPUTS: [(&str, &str, &str, &str); 16] = [
    (
        "cross",
        "a",
        "ps.gamepad.inputs.cross.label",
        "ps.gamepad.inputs.cross.action",
    ),
    (
        "circle",
        "b",
        "ps.gamepad.inputs.circle.label",
        "ps.gamepad.inputs.circle.action",
    ),
    (
        "square",
        "x",
        "ps.gamepad.inputs.square.label",
        "ps.gamepad.inputs.square.action",
    ),
    (
        "triangle",
        "y",
        "ps.gamepad.inputs.triangle.label",
        "ps.gamepad.inputs.triangle.action",
    ),
    (
        "l1",
        "leftshoulder",
        "ps.gamepad.inputs.l1.label",
        "ps.gamepad.inputs.l1.action",
    ),
    (
        "r1",
        "rightshoulder",
        "ps.gamepad.inputs.r1.label",
        "ps.gamepad.inputs.r1.action",
    ),
    (
        "l2",
        "lefttrigger",
        "ps.gamepad.inputs.l2.label",
        "ps.gamepad.inputs.l2.action",
    ),
    (
        "r2",
        "righttrigger",
        "ps.gamepad.inputs.r2.label",
        "ps.gamepad.inputs.r2.action",
    ),
    (
        "l3",
        "leftstick",
        "ps.gamepad.inputs.l3.label",
        "ps.gamepad.inputs.l3.action",
    ),
    (
        "r3",
        "rightstick",
        "ps.gamepad.inputs.r3.label",
        "ps.gamepad.inputs.r3.action",
    ),
    (
        "options",
        "start",
        "ps.gamepad.inputs.options.label",
        "ps.gamepad.inputs.options.action",
    ),
    (
        "touchpad",
        "back,touchpad",
        "ps.gamepad.inputs.touchpad.label",
        "ps.gamepad.inputs.touchpad.action",
    ),
    (
        "up",
        "dpup",
        "ps.gamepad.inputs.up.label",
        "ps.gamepad.inputs.up.action",
    ),
    (
        "down",
        "dpdown",
        "ps.gamepad.inputs.down.label",
        "ps.gamepad.inputs.down.action",
    ),
    (
        "left",
        "dpleft",
        "ps.gamepad.inputs.left.label",
        "ps.gamepad.inputs.left.action",
    ),
    (
        "right",
        "dpright",
        "ps.gamepad.inputs.right.label",
        "ps.gamepad.inputs.right.action",
    ),
];

/// psport's `pad.menu`, in [`INPUTS`]' shape: the port's own menu.
///
/// psport opens it on L3+R3 whatever is bound, so the default writes no
/// line. Kept out of [`PadLayout::assign`]'s swaps since the game may keep
/// the same button.
pub const MENU: (&str, &str, &str, &str) = (
    "menu",
    "leftstick+rightstick",
    "ps.gamepad.inputs.menu.label",
    "ps.gamepad.inputs.menu.action",
);

/// Two-button chords `pad.menu` takes besides [`BUTTONS`], with the app's
/// locale key of their label.
pub const MENU_CHORDS: [(&str, &str); 1] = [("leftstick+rightstick", "ps.gamepad.buttons.l3_r3")];

/// SDL's gamepad button names bbport accepts, with the app's locale key of
/// how a pad labels them.
pub const BUTTONS: [(&str, &str); 21] = [
    ("a", "ps.gamepad.buttons.a"),
    ("b", "ps.gamepad.buttons.b"),
    ("x", "ps.gamepad.buttons.x"),
    ("y", "ps.gamepad.buttons.y"),
    ("leftshoulder", "ps.gamepad.buttons.leftshoulder"),
    ("rightshoulder", "ps.gamepad.buttons.rightshoulder"),
    ("lefttrigger", "ps.gamepad.buttons.lefttrigger"),
    ("righttrigger", "ps.gamepad.buttons.righttrigger"),
    ("leftstick", "ps.gamepad.buttons.leftstick"),
    ("rightstick", "ps.gamepad.buttons.rightstick"),
    ("back", "ps.gamepad.buttons.back"),
    ("start", "ps.gamepad.buttons.start"),
    ("touchpad", "ps.gamepad.buttons.touchpad"),
    ("dpup", "ps.gamepad.buttons.dpup"),
    ("dpdown", "ps.gamepad.buttons.dpdown"),
    ("dpleft", "ps.gamepad.buttons.dpleft"),
    ("dpright", "ps.gamepad.buttons.dpright"),
    ("misc1", "ps.gamepad.buttons.misc1"),
    ("paddle1", "ps.gamepad.buttons.paddle1"),
    ("paddle2", "ps.gamepad.buttons.paddle2"),
    ("paddle3", "ps.gamepad.buttons.paddle3"),
];

fn default_buttons(input: &str) -> &'static str {
    INPUTS
        .iter()
        .chain([&MENU])
        .find(|(name, ..)| *name == input)
        .map_or("", |(_, buttons, ..)| buttons)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PadLayout {
    pub id: String,
    pub name: String,
    /// `(input, SDL buttons)` for the inputs moved off their default.
    pub lines: Vec<(String, String)>,
    /// The shipped layout a user's own one was duplicated from, which a
    /// restore goes back to; empty for bbport's own mapping.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub template: String,
    /// Shipped with decanter: shown, applied and duplicated, never edited.
    #[serde(skip)]
    pub builtin: bool,
}

impl PadLayout {
    pub fn builtins() -> [Self; 2] {
        let lines = |pairs: &[(&str, &str)]| {
            pairs
                .iter()
                .map(|(input, buttons)| ((*input).to_owned(), (*buttons).to_owned()))
                .collect()
        };
        [
            Self {
                id: "default".to_owned(),
                name: "Predeterminado".to_owned(),
                lines: Vec::new(),
                template: String::new(),
                builtin: true,
            },
            // Code Vein's fighting buttons on Bloodborne's closest actions;
            // the vial and items take the buttons Code Vein gives to dash
            // and the gift menu, which Bloodborne has no use for.
            Self {
                id: "code-vein".to_owned(),
                name: "Estilo Code Vein".to_owned(),
                lines: lines(&[
                    ("r1", "x"),
                    ("r2", "y"),
                    ("circle", "a"),
                    ("cross", "b"),
                    ("triangle", "rightshoulder"),
                    ("square", "righttrigger"),
                ]),
                template: String::new(),
                builtin: true,
            },
        ]
    }

    /// The buttons `input` answers to under this layout.
    pub fn buttons(&self, input: &str) -> &str {
        self.lines
            .iter()
            .find(|(name, _)| name == input)
            .map_or_else(|| default_buttons(input), |(_, buttons)| buttons.as_str())
    }

    /// Puts `button` on `input`. The input that had it gives it up: when it
    /// was its only button it takes `input`'s old ones, so the two swap and
    /// no button ends up doing two things or none.
    pub fn assign(&mut self, input: &str, button: &str) {
        if input == MENU.0 {
            self.set(input, button);
            return;
        }
        let previous = self.buttons(input).to_owned();
        let holder = INPUTS
            .iter()
            .map(|(name, ..)| *name)
            .find(|other| *other != input && self.buttons(other).split(',').any(|held| held == button));
        if let Some(holder) = holder {
            let held = self.buttons(holder);
            let rest: Vec<&str> = held.split(',').filter(|held| *held != button).collect();
            let replacement = if rest.is_empty() { previous } else { rest.join(",") };
            self.set(holder, &replacement);
        }
        self.set(input, button);
    }

    /// The id of the shipped layout this one restores to: itself when it
    /// is one, else the one it was duplicated from.
    pub fn template(&self) -> &str {
        if self.builtin { &self.id } else { &self.template }
    }

    /// Puts the inputs `scope` picks back on the template's buttons and
    /// leaves the rest as they are.
    pub fn restore(&mut self, scope: impl Fn(&str) -> bool) {
        let template = Self::builtins()
            .into_iter()
            .find(|layout| layout.id == self.template())
            .map(|layout| layout.lines)
            .unwrap_or_default();
        self.lines.retain(|(input, _)| !scope(input));
        self.lines
            .extend(template.into_iter().filter(|(input, _)| scope(input)));
        self.lines.sort();
    }

    fn set(&mut self, input: &str, buttons: &str) {
        self.lines.retain(|(name, _)| name != input);
        if buttons != default_buttons(input) {
            self.lines.push((input.to_owned(), buttons.to_owned()));
            self.lines.sort();
        }
    }

    fn matches(&self, ini: &Ini) -> bool {
        let mut written: Vec<(&str, &str)> = ini
            .prefixed(PREFIX)
            .map(|(key, value)| (&key[PREFIX.len()..], value))
            .collect();
        written.sort_unstable();
        let mut own: Vec<(&str, &str)> = self
            .lines
            .iter()
            .map(|(input, buttons)| (input.as_str(), buttons.as_str()))
            .collect();
        own.sort_unstable();
        own == written
    }

    fn apply(&self, ini: &mut Ini) {
        let stale: Vec<String> = ini.prefixed(PREFIX).map(|(key, _)| key.to_owned()).collect();
        for key in stale {
            ini.remove(&key);
        }
        for (input, buttons) in &self.lines {
            ini.set(&format!("{PREFIX}{input}"), buttons);
        }
    }

    /// Against `bbport.ini` as it is now, like every other edit of it. File
    /// I/O: off the UI thread.
    pub fn save_to_ini(&self, root: &Root) -> io::Result<()> {
        let path = Bbport::ini_path(root);
        let mut ini = Ini::load(&path);
        self.apply(&mut ini);
        ini.save(&path)
    }
}

/// What `bbport.ini` holds now as an unsaved layout, so a hand-edited set
/// of `pad.` lines can be copied into one. File I/O: off the UI thread.
pub fn from_ini(root: &Root) -> PadLayout {
    let ini = Ini::load(&Bbport::ini_path(root));
    let mut lines: Vec<(String, String)> = ini
        .prefixed(PREFIX)
        .map(|(key, value)| (key[PREFIX.len()..].to_owned(), value.to_owned()))
        .collect();
    lines.sort();
    PadLayout {
        id: String::new(),
        name: "Personalizado".to_owned(),
        lines,
        template: String::new(),
        builtin: true,
    }
}

/// The id of the layout among `layouts` that `bbport.ini` holds now, `None`
/// when its `pad.` lines match none. File I/O: off the UI thread.
pub fn current<'a>(root: &Root, layouts: impl IntoIterator<Item = &'a PadLayout>) -> Option<String> {
    let ini = Ini::load(&Bbport::ini_path(root));
    layouts
        .into_iter()
        .find(|layout| layout.matches(&ini))
        .map(|layout| layout.id.clone())
}
