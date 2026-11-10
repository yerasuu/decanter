// SPDX-License-Identifier: GPL-3.0-or-later
//! What applying a pack would do, as data the plan dialog shows and the apply loop walks (design,
//! section 5.3). Failure is skip and flag (decision #1635): an entry that can't be had holds back
//! only what requires it, its slot stays reserved so its mod lands between its neighbours whenever
//! it arrives, and running apply again recomputes everything from what is installed.

use std::collections::HashMap;

use decanter_mods::ModId;

use crate::{Diff, Entry, EntryKey, Pack, Resolution, ResolveError, Status};

/// What the app, which talks to providers, says it can do for an entry the game lacks or has at
/// another version.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Availability {
    /// decanter can download it.
    Fetch,
    /// The user must provide the file: a Nexus free account, a local archive not given yet, mod.io
    /// before its login.
    Manual,
    /// The provider no longer lists it, its version or file is gone, or its provider is not mapped.
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum StepState {
    Have {
        id: ModId,
        adopted: bool,
    },
    /// `replaces` is an installed other version whose slot the new one takes.
    Fetch {
        replaces: Option<ModId>,
    },
    Manual {
        replaces: Option<ModId>,
    },
    Unavailable {
        installed: Option<ModId>,
    },
    /// Its download, check or import failed during this run.
    Failed {
        installed: Option<ModId>,
    },
}

impl StepState {
    /// Whether the entry's mod will not be there at its pin once the run ends.
    const fn is_waiting(&self) -> bool {
        matches!(
            self,
            Self::Manual { .. } | Self::Unavailable { .. } | Self::Failed { .. }
        )
    }
}

#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct Step {
    pub key: EntryKey,
    pub state: StepState,
    /// The first requirement that won't be there, directly or through its own requirements: this
    /// step is not fetched, and a mod of it already present is left disabled.
    pub blocked_by: Option<EntryKey>,
    /// Whether the entry's mod ends enabled at its slot: the pack enables it, it is not blocked or
    /// waiting, and every requirement ends enabled. A step that doesn't run changes nothing.
    pub enable: bool,
    /// The requirement that ends disabled and so keeps an entry the pack enables off.
    pub held_by: Option<EntryKey>,
}

/// The steps in the resolved sequence, which is also the install sequence: one per entry, a waiting
/// one included, since its position is the slot reserved for its mod.
#[derive(Debug, Clone)]
pub struct Plan {
    steps: Vec<Step>,
    requirements: Vec<Vec<usize>>,
    wanted: Vec<bool>,
}

impl Plan {
    /// `resolution` and `diff` must be of `pack`; `availability` is asked only for entries the game
    /// lacks or has at another version than the pin.
    ///
    /// # Errors
    ///
    /// [`ResolveError::UnknownEntry`] for a resolved key `pack` does not have.
    #[inline]
    pub fn new(
        pack: &Pack,
        resolution: &Resolution,
        diff: &Diff,
        mut availability: impl FnMut(&Entry) -> Availability,
    ) -> Result<Self, ResolveError> {
        let entries: HashMap<&EntryKey, &Entry> = pack.entries.iter().map(|entry| (&entry.key, entry)).collect();
        let mut steps = Vec::with_capacity(resolution.sequence().len());
        let mut wanted = Vec::with_capacity(resolution.sequence().len());
        for key in resolution.sequence() {
            let entry = entries
                .get(key)
                .ok_or_else(|| ResolveError::UnknownEntry(key.clone()))?;
            let state = match diff.status(key) {
                Some(Status::Present { id, adopted }) => StepState::Have {
                    id: id.clone(),
                    adopted: *adopted,
                },
                Some(Status::Outdated { id, .. }) => waiting(availability(entry), Some(id.clone())),
                Some(Status::Missing) | None => waiting(availability(entry), None),
            };
            steps.push(Step {
                key: key.clone(),
                state,
                blocked_by: None,
                enable: false,
                held_by: None,
            });
            wanted.push(entry.enabled);
        }
        let mut plan = Self {
            steps,
            requirements: resolution.requirements().to_vec(),
            wanted,
        };
        plan.settle();
        Ok(plan)
    }

    #[must_use]
    #[inline]
    pub fn steps(&self) -> &[Step] {
        &self.steps
    }

    /// Entries the pack still waits for, blocked ones included: *Incomplete: N of M waiting*.
    #[must_use]
    #[inline]
    pub fn waiting(&self) -> usize {
        self.steps
            .iter()
            .filter(|step| step.blocked_by.is_some() || step.state.is_waiting())
            .count()
    }

    /// Once `key`'s mod is imported during the run. `false` when no step has that key.
    #[inline]
    pub fn installed(&mut self, key: &EntryKey, id: ModId) -> bool {
        self.update(key, |_| StepState::Have { id, adopted: false })
    }

    /// Once `key`'s download, check or import fails: the run goes on, and what requires it is
    /// blocked. `false` when no step has that key.
    #[inline]
    pub fn failed(&mut self, key: &EntryKey) -> bool {
        self.update(key, |state| StepState::Failed {
            installed: match state {
                StepState::Have { id, .. } => Some(id),
                StepState::Fetch { replaces } | StepState::Manual { replaces } => replaces,
                StepState::Unavailable { installed } | StepState::Failed { installed } => installed,
            },
        })
    }

    /// The mod a newly arrived `key` is placed right after: that of the nearest earlier step that
    /// has one (design, section 5.5). `None` puts it at the start of the pack's block.
    #[must_use]
    #[inline]
    pub fn anchor(&self, key: &EntryKey) -> Option<&ModId> {
        let index = self.steps.iter().position(|step| step.key == *key)?;
        self.steps
            .get(..index)?
            .iter()
            .rev()
            .find_map(|step| match &step.state {
                StepState::Have { id, .. } => Some(id),
                StepState::Fetch { .. }
                | StepState::Manual { .. }
                | StepState::Unavailable { .. }
                | StepState::Failed { .. } => None,
            })
    }

    fn update(&mut self, key: &EntryKey, change: impl FnOnce(StepState) -> StepState) -> bool {
        let Some(step) = self.steps.iter_mut().find(|step| step.key == *key) else {
            return false;
        };
        let previous = core::mem::replace(&mut step.state, StepState::Fetch { replaces: None });
        step.state = change(previous);
        self.settle();
        true
    }

    /// One pass in sequence order, since every requirement comes before what requires it.
    fn settle(&mut self) {
        for index in 0..self.steps.len() {
            let requirements = self.requirements.get(index).map_or(&[][..], Vec::as_slice);
            let earlier = self.steps.get(..index).unwrap_or_default();
            let required = || requirements.iter().filter_map(|&position| earlier.get(position));
            let blocked_by = required()
                .find(|step| step.blocked_by.is_some() || step.state.is_waiting())
                .map(|step| step.key.clone());
            let held_by = required().find(|step| !step.enable).map(|step| step.key.clone());
            let is_wanted = self.wanted.get(index).copied().unwrap_or(false);
            let Some(step) = self.steps.get_mut(index) else {
                continue;
            };
            let runs = blocked_by.is_none() && !step.state.is_waiting();
            step.enable = is_wanted && runs && held_by.is_none();
            step.held_by = if is_wanted && runs { held_by } else { None };
            step.blocked_by = blocked_by;
        }
    }
}

const fn waiting(availability: Availability, installed: Option<ModId>) -> StepState {
    match availability {
        Availability::Fetch => StepState::Fetch { replaces: installed },
        Availability::Manual => StepState::Manual { replaces: installed },
        Availability::Unavailable => StepState::Unavailable { installed },
    }
}
