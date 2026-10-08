//! `bbport.ini`, read by bbport itself and by its in-game menu, and the
//! keyboard bindings it carries as `key.<input>=` lines.

use std::collections::HashMap;
use std::fs;
use std::path::Path;

use crate::mapping::{Action, Mapping, Source, Target};

/// `key=value` lines. Edited keys are rewritten in place and new ones appended, comments and
/// unknown keys stay, the way bbport's own launcher saved it.
#[derive(Clone, Default)]
pub struct Ini {
    lines: Vec<String>,
    values: HashMap<String, String>,
}

impl Ini {
    pub fn load(path: &Path) -> Ini {
        let lines: Vec<String> = fs::read_to_string(path)
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect();
        let values = lines
            .iter()
            .filter(|line| !line.trim_start().starts_with('#'))
            .filter_map(|line| line.split_once('='))
            .map(|(k, v)| (k.trim().to_owned(), v.trim().to_owned()))
            .collect();
        Ini { lines, values }
    }

    pub fn get(&self, key: &str) -> Option<&str> {
        self.values.get(key).map(String::as_str)
    }

    pub fn set(&mut self, key: &str, value: &str) {
        self.values.insert(key.to_owned(), value.to_owned());
        let line = format!("{key}={value}");
        let existing = self
            .lines
            .iter_mut()
            .find(|l| !l.trim_start().starts_with('#') && l.split_once('=').is_some_and(|(k, _)| k.trim() == key));
        match existing {
            Some(existing) => *existing = line,
            None => self.lines.push(line),
        }
    }

    /// Drops the key's lines, so the port falls back to its default.
    pub fn remove(&mut self, key: &str) {
        self.values.remove(key);
        self.lines
            .retain(|l| l.trim_start().starts_with('#') || l.split_once('=').is_none_or(|(k, _)| k.trim() != key));
    }

    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        crate::files::write_atomic(path, (self.lines.join("\n") + "\n").as_bytes())
    }
}

/// Keyboard bindings from `key.<input>=` lines (upstream bbport `src/runtime_pad.c`,
/// `load_bindings`): a line replaces the input's keys, an empty one leaves it unbound.
pub fn read_binds(ini: &Ini, defaults: &Mapping) -> Mapping {
    let mut mapping = defaults.clone();
    for action in Action::ALL {
        let Some(input) = Target::bbport_input(action) else {
            continue;
        };
        if let Some(keys) = ini.get(&format!("key.{input}")) {
            let sources = keys.split(',').map(str::trim).filter(|k| !k.is_empty());
            mapping.set(action, sources.map(|k| Source::Key(k.to_owned())).collect());
        }
    }
    mapping
}

/// Writes `key.` lines only for inputs that differ from the defaults, so later default changes
/// upstream still reach the rest.
pub fn write_binds(ini: &mut Ini, mapping: &Mapping, defaults: &Mapping) {
    for action in Action::ALL {
        let Some(input) = Target::bbport_input(action) else {
            continue;
        };
        let key = format!("key.{input}");
        if mapping.get(action) == defaults.get(action) {
            ini.remove(&key);
            continue;
        }
        let names: Vec<&str> = mapping
            .get(action)
            .iter()
            .filter_map(|source| match source {
                Source::Key(name) => Some(name.as_str()),
                _ => None,
            })
            .collect();
        ini.set(&key, &names.join(","));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ini_edits_in_place_and_keeps_the_rest() {
        let path = std::env::temp_dir().join(format!("decanter-ps-ini-{}.ini", std::process::id()));
        fs::write(&path, "# note\nupscaler=fsr3\nunknown=7\n").unwrap();
        let mut ini = Ini::load(&path);
        assert_eq!(ini.get("upscaler"), Some("fsr3"));
        ini.set("upscaler", "fsr4");
        ini.set("preset", "2");
        ini.save(&path).unwrap();
        let text = fs::read_to_string(&path).unwrap();
        fs::remove_file(&path).unwrap();
        assert_eq!(text, "# note\nupscaler=fsr4\nunknown=7\npreset=2\n");
    }

    #[test]
    fn binds_round_trip_through_ini_lines() {
        let path = std::env::temp_dir().join(format!("decanter-ps-binds-{}.ini", std::process::id()));
        fs::write(&path, "upscaler=fsr4\nkey.cross=F, Space\nkey.l3=\n").unwrap();
        let defaults = Target::Bbport.defaults();
        let mut ini = Ini::load(&path);
        let mut mapping = read_binds(&ini, &defaults);
        assert_eq!(
            mapping.get(Action::Cross),
            [Source::Key("F".into()), Source::Key("Space".into())]
        );
        assert!(mapping.get(Action::L3).is_empty());
        mapping.reset(Action::Cross, &defaults);
        mapping.set(Action::LeftStickUp, vec![Source::Key("Up".into())]);
        write_binds(&mut ini, &mapping, &defaults);
        ini.save(&path).unwrap();
        let text = fs::read_to_string(&path).unwrap();
        fs::remove_file(&path).unwrap();
        assert_eq!(text, "upscaler=fsr4\nkey.l3=\nkey.move_up=Up\n");
    }
}
