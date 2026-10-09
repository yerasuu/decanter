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

const PREFIX: &str = "pad.";

/// `(input, its default SDL buttons, PS4 label, what Bloodborne does with
/// it)`. Defaults as `bind_defaults` in bbport's `src/runtime_pad.c`.
pub const INPUTS: [(&str, &str, &str, &str); 16] = [
    ("cross", "a", "✕", "Interactuar"),
    ("circle", "b", "○", "Esquivar · correr (mantener)"),
    ("square", "x", "□", "Usar objeto"),
    ("triangle", "y", "△", "Vial de sangre"),
    ("l1", "leftshoulder", "L1", "Transformar arma"),
    ("r1", "rightshoulder", "R1", "Ataque"),
    ("l2", "lefttrigger", "L2", "Disparo · parry"),
    ("r2", "righttrigger", "R2", "Ataque fuerte · cargado"),
    ("l3", "leftstick", "L3", "L3"),
    ("r3", "rightstick", "R3", "Fijar objetivo"),
    ("options", "start", "Options", "Menú"),
    ("touchpad", "back,touchpad", "Panel táctil", "Gestos"),
    ("up", "dpup", "Cruceta ↑", "Cambiar objeto rápido"),
    ("down", "dpdown", "Cruceta ↓", "Cruceta abajo"),
    ("left", "dpleft", "Cruceta ←", "Cambiar arma izquierda"),
    ("right", "dpright", "Cruceta →", "Cambiar arma derecha"),
];

/// SDL's gamepad button names bbport accepts, with how a pad labels them.
pub const BUTTONS: [(&str, &str); 21] = [
    ("a", "A / ✕"),
    ("b", "B / ○"),
    ("x", "X / □"),
    ("y", "Y / △"),
    ("leftshoulder", "LB / L1"),
    ("rightshoulder", "RB / R1"),
    ("lefttrigger", "LT / L2"),
    ("righttrigger", "RT / R2"),
    ("leftstick", "Clic stick izq."),
    ("rightstick", "Clic stick der."),
    ("back", "Select / Back"),
    ("start", "Start"),
    ("touchpad", "Panel táctil"),
    ("dpup", "Cruceta ↑"),
    ("dpdown", "Cruceta ↓"),
    ("dpleft", "Cruceta ←"),
    ("dpright", "Cruceta →"),
    ("misc1", "Misc"),
    ("paddle1", "Pala 1"),
    ("paddle2", "Pala 2"),
    ("paddle3", "Pala 3"),
];

fn default_buttons(input: &str) -> &'static str {
    INPUTS
        .iter()
        .find(|(name, ..)| *name == input)
        .map_or("", |(_, buttons, ..)| buttons)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PadLayout {
    pub id: String,
    pub name: String,
    /// `(input, SDL buttons)` for the inputs moved off their default.
    pub lines: Vec<(String, String)>,
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
    pub fn save_to_ini(&self) -> io::Result<()> {
        let path = Bbport::ini_path();
        let mut ini = Ini::load(&path);
        self.apply(&mut ini);
        ini.save(&path)
    }
}

/// What `bbport.ini` holds now as an unsaved layout, so a hand-edited set
/// of `pad.` lines can be copied into one. File I/O: off the UI thread.
pub fn from_ini() -> PadLayout {
    let ini = Ini::load(&Bbport::ini_path());
    let mut lines: Vec<(String, String)> = ini
        .prefixed(PREFIX)
        .map(|(key, value)| (key[PREFIX.len()..].to_owned(), value.to_owned()))
        .collect();
    lines.sort();
    PadLayout {
        id: String::new(),
        name: "Personalizado".to_owned(),
        lines,
        builtin: true,
    }
}

/// The id of the layout among `layouts` that `bbport.ini` holds now, `None`
/// when its `pad.` lines match none. File I/O: off the UI thread.
pub fn current<'a>(layouts: impl IntoIterator<Item = &'a PadLayout>) -> Option<String> {
    let ini = Ini::load(&Bbport::ini_path());
    layouts
        .into_iter()
        .find(|layout| layout.matches(&ini))
        .map(|layout| layout.id.clone())
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;

    fn no_button_twice(layout: &PadLayout) {
        let mut seen = HashSet::new();
        for (input, ..) in INPUTS {
            for button in layout.buttons(input).split(',') {
                assert!(seen.insert(button), "{}: {button} bound twice", layout.id);
            }
        }
    }

    #[test]
    fn builtins_bind_each_button_once() {
        for layout in PadLayout::builtins() {
            no_button_twice(&layout);
        }
    }

    #[test]
    fn assigning_swaps_and_returning_to_default_drops_the_line() {
        let [mut layout, _] = PadLayout::builtins();
        layout.assign("r1", "x");
        assert_eq!((layout.buttons("r1"), layout.buttons("square")), ("x", "rightshoulder"));
        no_button_twice(&layout);
        layout.assign("touchpad", "y");
        assert_eq!(layout.buttons("triangle"), "back,touchpad");
        no_button_twice(&layout);
        layout.assign("triangle", "y");
        layout.assign("touchpad", "back");
        layout.assign("touchpad", "touchpad");
        layout.assign("r1", "rightshoulder");
        assert!(
            layout.lines.iter().all(|(input, _)| input == "touchpad"),
            "{:?}",
            layout.lines
        );
    }

    #[test]
    fn applying_replaces_every_pad_line_and_is_recognised() {
        let mut ini = Ini::default();
        ini.set("upscaler", "fsr4");
        ini.set("pad.l3", "leftstick,misc1");
        let [default, code_vein] = PadLayout::builtins();
        assert!(!default.matches(&ini) && !code_vein.matches(&ini));
        code_vein.apply(&mut ini);
        assert!(code_vein.matches(&ini));
        assert_eq!(ini.get("pad.l3"), None);
        default.apply(&mut ini);
        assert!(default.matches(&ini));
        assert_eq!(ini.get("upscaler"), Some("fsr4"));
    }
}
