//! The `anyps5-input.ini` format shared by AnyPS5 and bbport.
//!
//! Rules mirror AnyPS5's loader (`anyps5/core/libs/prx/libSceVideoOut/src/InputMapping.cpp`) and
//! bbport's (`bbport/src/runtime_pad.c`): the first line for an action replaces its built-in
//! bindings, later lines add alternates, duplicates are dropped and omitted actions keep their
//! built-in bindings. Key names are SDL scancode names, which only the ports can fully check.

use std::fmt;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Action {
    Cross,
    Circle,
    Triangle,
    Square,
    L1,
    R1,
    L2,
    R2,
    L3,
    R3,
    Options,
    Up,
    Right,
    Down,
    Left,
    LeftStickLeft,
    LeftStickRight,
    LeftStickUp,
    LeftStickDown,
    RightStickLeft,
    RightStickRight,
    RightStickUp,
    RightStickDown,
    TouchLeft,
    TouchRight,
    ToggleMouse,
    ToggleFullscreen,
}

impl Action {
    pub const ALL: [Action; 27] = [
        Action::Cross,
        Action::Circle,
        Action::Triangle,
        Action::Square,
        Action::L1,
        Action::R1,
        Action::L2,
        Action::R2,
        Action::L3,
        Action::R3,
        Action::Options,
        Action::Up,
        Action::Right,
        Action::Down,
        Action::Left,
        Action::LeftStickLeft,
        Action::LeftStickRight,
        Action::LeftStickUp,
        Action::LeftStickDown,
        Action::RightStickLeft,
        Action::RightStickRight,
        Action::RightStickUp,
        Action::RightStickDown,
        Action::TouchLeft,
        Action::TouchRight,
        Action::ToggleMouse,
        Action::ToggleFullscreen,
    ];

    /// The name written in the file.
    pub fn name(self) -> &'static str {
        match self {
            Action::Cross => "Cross",
            Action::Circle => "Circle",
            Action::Triangle => "Triangle",
            Action::Square => "Square",
            Action::L1 => "L1",
            Action::R1 => "R1",
            Action::L2 => "L2",
            Action::R2 => "R2",
            Action::L3 => "L3",
            Action::R3 => "R3",
            Action::Options => "Options",
            Action::Up => "Up",
            Action::Right => "Right",
            Action::Down => "Down",
            Action::Left => "Left",
            Action::LeftStickLeft => "LeftStickLeft",
            Action::LeftStickRight => "LeftStickRight",
            Action::LeftStickUp => "LeftStickUp",
            Action::LeftStickDown => "LeftStickDown",
            Action::RightStickLeft => "RightStickLeft",
            Action::RightStickRight => "RightStickRight",
            Action::RightStickUp => "RightStickUp",
            Action::RightStickDown => "RightStickDown",
            Action::TouchLeft => "TouchLeft",
            Action::TouchRight => "TouchRight",
            Action::ToggleMouse => "ToggleMouse",
            Action::ToggleFullscreen => "ToggleFullscreen",
        }
    }

    fn from_name(name: &str) -> Option<Action> {
        Action::ALL
            .into_iter()
            .find(|action| action.name().eq_ignore_ascii_case(name))
    }

    fn index(self) -> usize {
        self as usize
    }

    /// Wheel steps are momentary presses, so both ports accept them only for pad buttons.
    fn is_button(self) -> bool {
        self.index() <= Action::Left.index()
    }

    /// Rejects sources the ports refuse for this action.
    pub fn check(self, source: &Source) -> Result<(), &'static str> {
        match source {
            Source::Mouse(_) if self == Action::ToggleFullscreen => Err("ToggleFullscreen can only use a keyboard key"),
            Source::Wheel(_) if !self.is_button() => Err("mouse wheel can only map to a pad button"),
            _ => Ok(()),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MouseButton {
    Left,
    Middle,
    Right,
    X1,
    X2,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Wheel {
    Up,
    Down,
}

#[derive(Clone, Debug)]
pub enum Source {
    /// An SDL scancode name such as `Space` or `Left Shift`.
    Key(String),
    Mouse(MouseButton),
    Wheel(Wheel),
}

/// Key names compare case-insensitively, like `SDL_GetScancodeFromName`, so a hand-written
/// `KEY:space` is the same binding as the app's `KEY:Space`.
impl PartialEq for Source {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Source::Key(a), Source::Key(b)) => a.eq_ignore_ascii_case(b),
            (Source::Mouse(a), Source::Mouse(b)) => a == b,
            (Source::Wheel(a), Source::Wheel(b)) => a == b,
            _ => false,
        }
    }
}

impl fmt::Display for Source {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Source::Key(name) => write!(f, "KEY:{name}"),
            Source::Mouse(button) => write!(f, "MOUSE:{button:?}"),
            Source::Wheel(direction) => write!(f, "WHEEL:{direction:?}"),
        }
    }
}

fn parse_source(text: &str) -> Result<Source, String> {
    let (kind, value) = text.split_once(':').ok_or("source must use TYPE:VALUE format")?;
    let value = value.trim();
    if value.is_empty() {
        return Err("source value is empty".into());
    }
    let pick = |options: &[(&str, Source)]| {
        options
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case(value))
            .map(|(_, source)| source.clone())
    };
    match kind.trim().to_ascii_uppercase().as_str() {
        "KEY" => Ok(Source::Key(value.to_owned())),
        "MOUSE" => pick(&[
            ("Left", Source::Mouse(MouseButton::Left)),
            ("Middle", Source::Mouse(MouseButton::Middle)),
            ("Right", Source::Mouse(MouseButton::Right)),
            ("X1", Source::Mouse(MouseButton::X1)),
            ("X2", Source::Mouse(MouseButton::X2)),
        ])
        .ok_or_else(|| format!("unknown mouse button '{value}'")),
        "WHEEL" => pick(&[("Up", Source::Wheel(Wheel::Up)), ("Down", Source::Wheel(Wheel::Down))])
            .ok_or_else(|| "mouse wheel direction must be UP or DOWN".into()),
        _ => Err("source type must be KEY, MOUSE or WHEEL".into()),
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct ParseError {
    pub line: usize,
    pub reason: String,
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "line {}: {}", self.line, self.reason)
    }
}

impl std::error::Error for ParseError {}

/// Bindings for every action, indexed by [`Action`].
#[derive(Clone, PartialEq, Debug)]
pub struct Mapping(Vec<Vec<Source>>);

impl Mapping {
    pub fn get(&self, action: Action) -> &[Source] {
        &self.0[action.index()]
    }

    /// Adds `source` unless the action already has it.
    pub fn add(&mut self, action: Action, source: Source) {
        let sources = &mut self.0[action.index()];
        if !sources.contains(&source) {
            sources.push(source);
        }
    }

    /// Removes one binding. Unless the target can store "unbound", the last one stays: in
    /// `anyps5-input.ini` an empty action silently falls back to the built-in binding.
    pub fn remove(&mut self, target: Target, action: Action, index: usize) {
        let sources = &mut self.0[action.index()];
        if (target.allows_unbound() || sources.len() > 1) && index < sources.len() {
            sources.remove(index);
        }
    }

    pub fn set(&mut self, action: Action, sources: Vec<Source>) {
        self.0[action.index()] = sources;
    }

    pub fn reset(&mut self, action: Action, defaults: &Mapping) {
        self.0[action.index()] = defaults.get(action).to_vec();
    }

    /// Applies a file over `defaults`.
    pub fn parse(text: &str, defaults: &Mapping) -> Result<Mapping, ParseError> {
        let mut mapping = defaults.clone();
        let mut overridden = [false; Action::ALL.len()];
        let text = text.strip_prefix('\u{feff}').unwrap_or(text);
        for (number, raw) in text.lines().enumerate() {
            let error = |reason: String| ParseError {
                line: number + 1,
                reason,
            };
            let content = raw.split(['#', ';']).next().unwrap_or_default().trim();
            if content.is_empty() {
                continue;
            }
            let (name, source) = content
                .split_once('=')
                .ok_or_else(|| error("expected Action = TYPE:VALUE".into()))?;
            let (name, source) = (name.trim(), source.trim());
            let action = Action::from_name(name).ok_or_else(|| error(format!("unknown action '{name}'")))?;
            if source.is_empty() {
                return Err(error("source is empty".into()));
            }
            let source = parse_source(source).map_err(error)?;
            action.check(&source).map_err(|reason| error(reason.into()))?;
            if !std::mem::replace(&mut overridden[action.index()], true) {
                mapping.0[action.index()].clear();
            }
            mapping.add(action, source);
        }
        Ok(mapping)
    }

    /// The file text: only actions that differ from `defaults`, so the file stays short and the
    /// port's later default changes still reach untouched actions.
    pub fn render(&self, defaults: &Mapping) -> String {
        let mut text = String::from("# Written by decanter. Format: Action = TYPE:VALUE\n");
        for action in Action::ALL {
            let sources = self.get(action);
            if sources == defaults.get(action) {
                continue;
            }
            for source in sources {
                text.push_str(&format!("{} = {source}\n", action.name()));
            }
        }
        text
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Target {
    Bbport,
    AnyPs5,
}

impl Target {
    /// bbport's input name for `key.<input>=` lines in `bbport.ini` (upstream
    /// `src/runtime_pad.c`, `input_names`); `None` for actions bbport has no input for.
    pub fn bbport_input(action: Action) -> Option<&'static str> {
        use Action::*;
        Some(match action {
            Cross => "cross",
            Circle => "circle",
            Square => "square",
            Triangle => "triangle",
            L1 => "l1",
            R1 => "r1",
            L2 => "l2",
            R2 => "r2",
            L3 => "l3",
            R3 => "r3",
            Options => "options",
            TouchLeft => "touchpad",
            TouchRight => "touchpad_right",
            Up => "up",
            Down => "down",
            Left => "left",
            Right => "right",
            LeftStickUp => "move_up",
            LeftStickDown => "move_down",
            LeftStickLeft => "move_left",
            LeftStickRight => "move_right",
            RightStickUp => "look_up",
            RightStickDown => "look_down",
            RightStickLeft => "look_left",
            RightStickRight => "look_right",
            ToggleMouse | ToggleFullscreen => return None,
        })
    }

    pub fn supports(self, action: Action) -> bool {
        self == Target::AnyPs5 || Self::bbport_input(action).is_some()
    }

    /// bbport's `key.` lines list keys only; AnyPS5 also takes mouse buttons and the wheel.
    pub fn check(self, action: Action, source: &Source) -> Result<(), &'static str> {
        if self == Target::Bbport && !matches!(source, Source::Key(_)) {
            return Err("bbport solo admite teclas del teclado");
        }
        action.check(source)
    }

    /// bbport's `key.<input>=` with nothing after it leaves the input unbound.
    pub fn allows_unbound(self) -> bool {
        self == Target::Bbport
    }

    /// The port's built-in bindings. Keep in sync with `bbport/src/runtime_pad.c`
    /// (`default_bindings`) and `anyps5/core/libs/prx/libScePad/include/InputMapping.hpp`.
    pub fn defaults(self) -> Mapping {
        use Action::*;
        let key = |name: &str| Source::Key(name.to_owned());
        let table: Vec<(Action, Source)> = match self {
            Target::Bbport => vec![
                (Cross, key("Space")),
                (Circle, key("Left Shift")),
                (Triangle, key("Q")),
                (Square, key("E")),
                (L1, key("1")),
                (R1, key("3")),
                (L2, key("R")),
                (R2, key("F")),
                (L3, key("Z")),
                (R3, key("C")),
                (Options, key("Return")),
                (Up, key("I")),
                (Right, key("L")),
                (Down, key("K")),
                (Left, key("J")),
                (LeftStickLeft, key("A")),
                (LeftStickRight, key("D")),
                (LeftStickUp, key("W")),
                (LeftStickDown, key("S")),
                (RightStickLeft, key("Left")),
                (RightStickRight, key("Right")),
                (RightStickUp, key("Up")),
                (RightStickDown, key("Down")),
                (TouchLeft, key("Tab")),
                (TouchRight, key("Backspace")),
            ],
            Target::AnyPs5 => vec![
                (ToggleFullscreen, key("F11")),
                (Cross, key("Return")),
                (Cross, key("Space")),
                (Options, key("Escape")),
                (Triangle, key("I")),
                (Circle, key("C")),
                (L1, key("Q")),
                (R1, key("E")),
                (R1, key("Left Alt")),
                (R1, key("Right Alt")),
                (L3, key("Left Shift")),
                (L3, key("Right Shift")),
                (R3, key("Left Ctrl")),
                (R3, key("Right Ctrl")),
                (Up, key("Up")),
                (Right, key("Right")),
                (Down, key("Down")),
                (Left, key("Left")),
                (LeftStickLeft, key("A")),
                (LeftStickRight, key("D")),
                (LeftStickUp, key("W")),
                (LeftStickDown, key("S")),
                (RightStickLeft, key("F")),
                (RightStickRight, key("H")),
                (RightStickUp, key("T")),
                (RightStickDown, key("G")),
                (TouchLeft, key("Backspace")),
                (TouchRight, key("Tab")),
                (ToggleMouse, Source::Mouse(MouseButton::Middle)),
                (Square, Source::Mouse(MouseButton::Left)),
                (R2, Source::Mouse(MouseButton::Right)),
                (Up, Source::Wheel(Wheel::Up)),
                (Down, Source::Wheel(Wheel::Down)),
            ],
        };
        let mut mapping = Mapping(vec![Vec::new(); Action::ALL.len()]);
        for (action, source) in table {
            mapping.add(action, source);
        }
        mapping
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_line_replaces_later_lines_add() {
        let defaults = Target::Bbport.defaults();
        let mapping = Mapping::parse(
            "\u{feff}# comment\ncross = KEY:F\nCross = key:space ; note\nCROSS = KEY:Space\nSquare = MOUSE:left\n",
            &defaults,
        )
        .unwrap();
        assert_eq!(
            mapping.get(Action::Cross),
            [Source::Key("F".into()), Source::Key("space".into())]
        );
        assert_eq!(mapping.get(Action::Square), [Source::Mouse(MouseButton::Left)]);
        assert_eq!(mapping.get(Action::Circle), defaults.get(Action::Circle));
    }

    #[test]
    fn rejects_what_the_ports_reject() {
        let defaults = Target::AnyPs5.defaults();
        let reason = |text| Mapping::parse(text, &defaults).unwrap_err();
        assert_eq!(reason("\nNope = KEY:A").line, 2);
        assert_eq!(reason("Cross KEY:A").reason, "expected Action = TYPE:VALUE");
        assert_eq!(
            reason("Cross = PAD:South").reason,
            "source type must be KEY, MOUSE or WHEEL"
        );
        assert_eq!(
            reason("ToggleFullscreen = MOUSE:Left").reason,
            "ToggleFullscreen can only use a keyboard key"
        );
        assert_eq!(
            reason("LeftStickUp = WHEEL:Up").reason,
            "mouse wheel can only map to a pad button"
        );
        assert_eq!(reason("R2 = MOUSE:Side").reason, "unknown mouse button 'Side'");
    }

    #[test]
    fn render_round_trips_and_skips_defaults() {
        for target in [Target::Bbport, Target::AnyPs5] {
            let defaults = target.defaults();
            assert_eq!(defaults.render(&defaults).lines().count(), 1);
            let mut mapping = defaults.clone();
            mapping.add(Action::R2, Source::Wheel(Wheel::Down));
            mapping.reset(Action::Cross, &defaults);
            mapping.add(Action::ToggleFullscreen, Source::Key("F10".into()));
            assert_eq!(Mapping::parse(&mapping.render(&defaults), &defaults).unwrap(), mapping);
        }
    }

    #[test]
    fn last_binding_is_kept_unless_unbound_is_storable() {
        let mut mapping = Target::AnyPs5.defaults();
        mapping.remove(Target::AnyPs5, Action::Options, 0);
        assert_eq!(mapping.get(Action::Options).len(), 1);
        let mut mapping = Target::Bbport.defaults();
        mapping.remove(Target::Bbport, Action::Cross, 0);
        assert!(mapping.get(Action::Cross).is_empty());
    }
}
