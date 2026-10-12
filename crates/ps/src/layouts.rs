//! Named control layouts: gamepad layouts for bbport and keyboard layouts
//! for each port, kept in decanter's PS data directory.
//!
//! The ones shipped with decanter are rebuilt from code every time and
//! never written; only the user's own are stored. Any layout can be
//! copied into a new, editable one. Which layout is in use is not stored:
//! it is read back from the port's own file, so a file edited elsewhere
//! shows as no layout rather than as one it no longer matches.

use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::bbport::pad::PadLayout;
use crate::mapping::{Mapping, Target};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeyLayout {
    pub id: String,
    pub name: String,
    /// `anyps5-input.ini` text over the port's defaults: only what differs.
    pub text: String,
    #[serde(skip)]
    pub builtin: bool,
}

impl KeyLayout {
    fn default_for() -> Self {
        Self {
            id: "default".to_owned(),
            name: "Predeterminado".to_owned(),
            text: String::new(),
            builtin: true,
        }
    }

    /// A stored text that no longer parses (edited by hand) gives the
    /// defaults rather than losing the layout.
    pub fn mapping(&self, target: Target) -> Mapping {
        let defaults = target.defaults();
        Mapping::parse(&self.text, &defaults).unwrap_or(defaults)
    }

    pub fn set_mapping(&mut self, target: Target, mapping: &Mapping) {
        self.text = mapping.render(&target.defaults());
    }
}

/// Cloned for each save, so the write happens off the UI thread on a
/// snapshot while the view keeps editing its own copy.
#[derive(Clone, Default, Serialize, Deserialize)]
pub struct Store {
    #[serde(default)]
    pad: Vec<PadLayout>,
    #[serde(default)]
    bbport_keys: Vec<KeyLayout>,
    #[serde(default)]
    anyps5_keys: Vec<KeyLayout>,
}

impl Store {
    fn path() -> PathBuf {
        crate::paths::active().join("layouts.json")
    }

    /// A missing file is an empty store; one that does not parse is an
    /// error, so it is never overwritten with nothing. File I/O: off the
    /// UI thread.
    pub fn load() -> io::Result<Self> {
        Self::load_from(&Self::path())
    }

    fn load_from(path: &Path) -> io::Result<Self> {
        match std::fs::read(path) {
            Ok(data) => serde_json::from_slice(&data).map_err(io::Error::other),
            Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(Self::default()),
            Err(err) => Err(err),
        }
    }

    pub fn save(&self) -> io::Result<()> {
        self.save_to(&Self::path())
    }

    fn save_to(&self, path: &Path) -> io::Result<()> {
        let text = serde_json::to_string_pretty(self).map_err(io::Error::other)? + "\n";
        crate::files::write_atomic(path, text.as_bytes())
    }

    /// The shipped layouts first, then the user's.
    pub fn pad_layouts(&self) -> Vec<PadLayout> {
        let mut layouts = Vec::with_capacity(self.pad.len() + 2);
        layouts.extend(PadLayout::builtins());
        layouts.extend(self.pad.iter().cloned());
        layouts
    }

    pub fn key_layouts(&self, target: Target) -> Vec<KeyLayout> {
        let own = self.keys(target);
        let mut layouts = Vec::with_capacity(own.len() + 1);
        layouts.push(KeyLayout::default_for());
        layouts.extend(own.iter().cloned());
        layouts
    }

    const fn keys(&self, target: Target) -> &Vec<KeyLayout> {
        match target {
            Target::Bbport => &self.bbport_keys,
            Target::AnyPs5 => &self.anyps5_keys,
        }
    }

    const fn keys_mut(&mut self, target: Target) -> &mut Vec<KeyLayout> {
        match target {
            Target::Bbport => &mut self.bbport_keys,
            Target::AnyPs5 => &mut self.anyps5_keys,
        }
    }

    /// `user-<n>`, past every id already used, so a deleted layout's id is
    /// never handed to a new one.
    fn next_id<'a>(ids: impl Iterator<Item = &'a str>) -> String {
        let last = ids
            .filter_map(|id| id.strip_prefix("user-")?.parse::<u32>().ok())
            .max()
            .unwrap_or(0);
        format!("user-{}", last.saturating_add(1))
    }

    /// An editable copy of `source` under `name`.
    pub fn copy_pad(&mut self, source: &PadLayout, name: String) -> PadLayout {
        let copy = PadLayout {
            id: Self::next_id(self.pad.iter().map(|layout| layout.id.as_str())),
            name,
            lines: source.lines.clone(),
            builtin: false,
        };
        self.pad.push(copy.clone());
        copy
    }

    /// An editable copy of `source` under `name`.
    pub fn copy_keys(&mut self, target: Target, source: &KeyLayout, name: String) -> KeyLayout {
        let keys = self.keys_mut(target);
        let copy = KeyLayout {
            id: Self::next_id(keys.iter().map(|layout| layout.id.as_str())),
            name,
            text: source.text.clone(),
            builtin: false,
        };
        keys.push(copy.clone());
        copy
    }

    /// Replaces the user layout with the same id; shipped ones are not stored.
    pub fn update_pad(&mut self, layout: &PadLayout) {
        if let Some(stored) = self.pad.iter_mut().find(|stored| stored.id == layout.id) {
            stored.clone_from(layout);
        }
    }

    pub fn update_keys(&mut self, target: Target, layout: &KeyLayout) {
        if let Some(stored) = self.keys_mut(target).iter_mut().find(|stored| stored.id == layout.id) {
            stored.clone_from(layout);
        }
    }

    pub fn remove_pad(&mut self, id: &str) {
        self.pad.retain(|layout| layout.id != id);
    }

    pub fn remove_keys(&mut self, target: Target, id: &str) {
        self.keys_mut(target).retain(|layout| layout.id != id);
    }
}

/// The id of the keyboard layout among `layouts` that `mapping` (read
/// from the port's file) equals, `None` when it equals none.
pub fn current_keys<'a>(
    target: Target,
    mapping: &Mapping,
    layouts: impl IntoIterator<Item = &'a KeyLayout>,
) -> Option<String> {
    layouts
        .into_iter()
        .find(|layout| layout.mapping(target) == *mapping)
        .map(|layout| layout.id.clone())
}
