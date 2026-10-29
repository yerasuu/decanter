//! The settings table's values, across the two files they live in.
//!
//! Each change is written on its own, against the files as they are on
//! disk then: the port ini is also written by the bindings editor and by
//! the port's in-game menu, and a save from a stale copy would undo theirs.

use std::io;
use std::path::{Path, PathBuf};

use super::groups::{Control, Setting, Store};
use super::{Bbport, Ini, IniScope, LauncherSettings};
use crate::paths::Root;

/// What a title's settings view shows, read once when it shows the title: its own port ini
/// ([`Bbport::title_ini`]) and the launch settings every title shares. File I/O: off the UI
/// thread.
#[derive(Clone)]
pub struct Values {
    title_id: String,
    ini_path: PathBuf,
    scope: IniScope,
    ini: Ini,
    launch: LauncherSettings,
}

impl Values {
    /// # Errors
    /// Making the title's port ini ([`Bbport::title_ini`]).
    pub fn load(root: &Root, title_id: &str) -> io::Result<Self> {
        let port_ini = Bbport::title_ini(root, title_id)?;
        Ok(Self {
            title_id: title_id.to_owned(),
            ini: Ini::load(&port_ini.path),
            ini_path: port_ini.path,
            scope: port_ini.scope,
            launch: LauncherSettings::load(root),
        })
    }

    /// The title these values were read for, which a save writes to.
    #[must_use]
    pub fn title_id(&self) -> &str {
        &self.title_id
    }

    /// The port ini the values were read from, which a save writes to.
    #[must_use]
    pub fn ini_path(&self) -> &Path {
        &self.ini_path
    }

    pub const fn scope(&self) -> IniScope {
        self.scope
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
                ..
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

    /// A port ini key outside the table, such as a title's own effect switch ([`super::effects`]).
    pub fn ini(&self, key: &str) -> Option<&str> {
        self.ini.get(key)
    }

    /// Shows `value` at once; [`save_ini`] writes it.
    pub fn set_ini(&mut self, key: &str, value: &str) {
        self.ini.set(key, value);
    }
}

fn apply(ini: &mut Ini, launch: &mut LauncherSettings, setting: &Setting, value: &str) {
    match setting.store {
        Store::Ini => ini.set(setting.key, value),
        Store::Launcher => apply_launch(launch, setting, value),
    }
}

fn apply_launch(launch: &mut LauncherSettings, setting: &Setting, value: &str) {
    match setting.control {
        Control::Toggle { .. } => launch.set_flag(setting.key, value == "1"),
        Control::Number { .. } => launch.set_number(setting.key, value.parse().unwrap_or_default()),
        Control::Choice { .. } | Control::Gamepad => launch.set_text(setting.key, value),
    }
}

/// Writes one setting into the files as they are now: a port ini key into `ini`, the title's
/// ([`Values::ini_path`]). A controller's name is kept beside its GUID, to show it while it is
/// unplugged.
pub fn save(root: &Root, ini: &Path, setting: &Setting, value: &str, gamepad_name: Option<&str>) -> io::Result<()> {
    match setting.store {
        Store::Ini => save_ini(ini, setting.key, value),
        // Through `edit`, so a settings file that can't be read or parsed fails the save instead
        // of being replaced by the defaults with this one change.
        Store::Launcher => LauncherSettings::edit(root, |launch| {
            apply_launch(launch, setting, value);
            if let Some(name) = gamepad_name {
                launch.set_text("gamepad_name", name);
            }
        }),
    }
}

/// [`save`] for a port ini key outside the table.
pub fn save_ini(path: &Path, key: &str, value: &str) -> io::Result<()> {
    let mut ini = Ini::load(path);
    ini.set(key, value);
    ini.save(path)
}
