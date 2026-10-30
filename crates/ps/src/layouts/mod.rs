//! Named control layouts: gamepad layouts for bbport and keyboard layouts for each port, a catalog
//! of templates shared by every game of the library (`docs/ps-per-game-settings.md`, D4), one file
//! per layout under `<library>/psport/layouts/` ([`catalog`]).
//!
//! The ones shipped with decanter are rebuilt from code every time and never written; only the
//! user's own are stored. Any layout can be copied into a new, editable one. Applying a layout copies
//! its lines into the game's own file, which is what the port reads, so editing a template changes
//! no game. Each game records which layout it took ([`choice`]); what its file holds beyond that
//! layout are its own changes.

mod catalog;
pub mod choice;

use std::io;

use serde::{Deserialize, Serialize};

pub use catalog::{put_keys, put_pad, remove};

use crate::bbport::pad::PadLayout;
use crate::mapping::{Mapping, Target};

/// One of the catalog's three lists.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Pad,
    Keys(Target),
}

impl Kind {
    pub const ALL: [Self; 3] = [Self::Pad, Self::Keys(Target::Bbport), Self::Keys(Target::AnyPs5)];

    /// The catalog's folder for this kind.
    const fn dir(self) -> &'static str {
        match self {
            Self::Pad => "pad",
            Self::Keys(Target::Bbport) => "bbport-keys",
            Self::Keys(Target::AnyPs5) => "anyps5-keys",
        }
    }
}

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

/// The catalog as read once, which a view edits in memory; each change is written on its own
/// ([`put_pad`], [`put_keys`], [`remove`]), so two views never overwrite each other's layouts.
#[derive(Clone, Default)]
pub struct Store {
    pad: Vec<PadLayout>,
    bbport_keys: Vec<KeyLayout>,
    anyps5_keys: Vec<KeyLayout>,
    /// Ids of files that could not be read, never handed to a new layout so that file is never
    /// overwritten.
    unreadable: Vec<(Kind, String)>,
}

impl Store {
    /// The catalog, made from the former `layouts.json` the first time ([`catalog`]). A layout file
    /// that doesn't parse is skipped and logged; a former file that doesn't parse is an error, so
    /// nothing is made from half of it. File I/O: off the UI thread.
    pub fn load() -> io::Result<Self> {
        catalog::load(&crate::paths::active())
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

    /// `user-<n>`, past every id of `kind` in the catalog, unreadable files included.
    fn next_id(&self, kind: Kind) -> String {
        let stored: Vec<&str> = match kind {
            Kind::Pad => self.pad.iter().map(|layout| layout.id.as_str()).collect(),
            Kind::Keys(target) => self.keys(target).iter().map(|layout| layout.id.as_str()).collect(),
        };
        let last = stored
            .into_iter()
            .chain(
                self.unreadable
                    .iter()
                    .filter(|(of, _)| *of == kind)
                    .map(|(_, id)| id.as_str()),
            )
            .filter_map(|id| id.strip_prefix("user-")?.parse::<u32>().ok())
            .max()
            .unwrap_or(0);
        format!("user-{}", last.saturating_add(1))
    }

    /// An editable copy of `source` under `name`, to be written with [`put_pad`].
    pub fn copy_pad(&mut self, source: &PadLayout, name: String) -> PadLayout {
        let copy = PadLayout {
            id: self.next_id(Kind::Pad),
            name,
            lines: source.lines.clone(),
            template: source.template().to_owned(),
            builtin: false,
        };
        self.pad.push(copy.clone());
        copy
    }

    /// An editable copy of `source` under `name`, to be written with [`put_keys`].
    pub fn copy_keys(&mut self, target: Target, source: &KeyLayout, name: String) -> KeyLayout {
        let copy = KeyLayout {
            id: self.next_id(Kind::Keys(target)),
            name,
            text: source.text.clone(),
            builtin: false,
        };
        self.keys_mut(target).push(copy.clone());
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

    fn has(&self, kind: Kind, id: &str) -> bool {
        match kind {
            Kind::Pad => self.pad_layouts().iter().any(|layout| layout.id == id),
            Kind::Keys(target) => self.key_layouts(target).iter().any(|layout| layout.id == id),
        }
    }

    /// The layout a game uses: the one it recorded ([`choice`]) while the catalog still has it,
    /// else the one its own lines equal (`matched`), as before games recorded theirs.
    pub fn in_use(
        &self,
        kind: Kind,
        recorded: Option<&str>,
        matched: impl FnOnce() -> Option<String>,
    ) -> Option<String> {
        recorded
            .filter(|id| self.has(kind, id))
            .map(str::to_owned)
            .or_else(matched)
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
