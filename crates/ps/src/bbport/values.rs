//! The settings table's values, across the two files they live in.
//!
//! Each change is written on its own, against the files as they are on
//! disk then: `bbport.ini` is also written by the bindings editor and by
//! bbport's in-game menu, and a save from a stale copy would undo theirs.

use std::io;

use super::groups::{Control, Setting, Store};
use super::{Bbport, Ini, LauncherSettings};
use crate::paths::Root;

/// What the settings view shows, read once when it opens. File I/O: off
/// the UI thread.
#[derive(Clone, Default)]
pub struct Values {
    ini: Ini,
    launch: LauncherSettings,
}

impl Values {
    pub fn load(root: &Root) -> Self {
        Self {
            ini: Ini::load(&Bbport::ini_path(root)),
            launch: LauncherSettings::load(root),
        }
    }

    pub const fn launch(&self) -> &LauncherSettings {
        &self.launch
    }

    /// The stored value as the table's controls spell it: `"1"`/`"0"` for
    /// toggles, the number's text, the choice's value. `None` when unset.
    pub fn get(&self, setting: &Setting) -> Option<String> {
        match setting.store {
            Store::Ini => self.ini.get(setting.key).map(str::to_owned),
            Store::Launcher => match &setting.control {
                Control::Number { .. } => self.launch.number(setting.key).map(|number| number.to_string()),
                Control::Toggle { default } => Some(
                    if self.launch.flag(setting.key, *default) {
                        "1"
                    } else {
                        "0"
                    }
                    .to_owned(),
                ),
                Control::Choice { .. } | Control::Gamepad => self.launch.text(setting.key).map(str::to_owned),
            },
        }
    }

    /// The value one click of ‹ › (or a switch) leads to. `gamepads` are
    /// the connected controllers, for [`Control::Gamepad`]; automatic
    /// (empty) comes first.
    pub fn stepped(&self, setting: &Setting, delta: isize, gamepads: &[(String, String)]) -> String {
        let current = self.get(setting);
        let wrap = |at: usize, len: usize| {
            let len = isize::try_from(len).unwrap_or(1).max(1);
            usize::try_from((isize::try_from(at).unwrap_or(0) + delta).rem_euclid(len)).unwrap_or(0)
        };
        match &setting.control {
            Control::Choice { options, default } => {
                let wanted = current.as_deref().unwrap_or(default);
                let at = options.iter().position(|option| option.1 == wanted).unwrap_or(0);
                options
                    .get(wrap(at, options.len()))
                    .map_or_else(String::new, |option| option.1.to_owned())
            }
            Control::Toggle { default } => {
                let on = current.map_or(*default, |value| value == "1");
                if on { "0" } else { "1" }.to_owned()
            }
            Control::Number {
                min,
                max,
                step,
                default,
                decimals,
            } => {
                let now = current.and_then(|value| value.parse::<f64>().ok()).unwrap_or(*default);
                #[expect(clippy::cast_precision_loss, reason = "delta is ±1")]
                let next = (now + step * delta as f64).clamp(*min, *max);
                format!("{next:.decimals$}")
            }
            Control::Gamepad => {
                let current = current.unwrap_or_default();
                let at = gamepads
                    .iter()
                    .position(|(guid, _)| *guid == current)
                    .map_or(0, |at| at + 1);
                let next = wrap(at, gamepads.len() + 1);
                next.checked_sub(1)
                    .and_then(|index| gamepads.get(index))
                    .map_or_else(String::new, |(guid, _)| guid.clone())
            }
        }
    }

    /// A launch setting outside the table, shown at once; the caller saves
    /// it with [`super::LauncherSettings::edit`].
    pub fn set_launch_text(&mut self, key: &str, value: &str) {
        self.launch.set_text(key, value);
    }

    /// Shows `value` at once; [`save`] writes it.
    pub fn set(&mut self, setting: &Setting, value: &str) {
        apply(&mut self.ini, &mut self.launch, setting, value);
    }
}

fn apply(ini: &mut Ini, launch: &mut LauncherSettings, setting: &Setting, value: &str) {
    match (setting.store, &setting.control) {
        (Store::Ini, _) => ini.set(setting.key, value),
        (Store::Launcher, Control::Toggle { .. }) => launch.set_flag(setting.key, value == "1"),
        (Store::Launcher, Control::Number { .. }) => launch.set_number(setting.key, value.parse().unwrap_or_default()),
        (Store::Launcher, Control::Choice { .. } | Control::Gamepad) => launch.set_text(setting.key, value),
    }
}

/// Writes one setting into the files as they are now. A controller's name
/// is kept beside its GUID, to show it while it is unplugged.
pub fn save(root: &Root, setting: &Setting, value: &str, gamepad_name: Option<&str>) -> io::Result<()> {
    let path = Bbport::ini_path(root);
    let mut ini = Ini::load(&path);
    let mut launch = LauncherSettings::load(root);
    apply(&mut ini, &mut launch, setting, value);
    if let Some(name) = gamepad_name {
        launch.set_text("gamepad_name", name);
    }
    match setting.store {
        Store::Ini => ini.save(&path),
        Store::Launcher => launch.save(root),
    }
}
