//! What moving a mod in the load order changes, so that the Mods tab asks before a move makes
//! another mod win a file or a DLL (mod packs design, section 5.4).

use std::collections::{BTreeMap, HashMap};

use crate::Result;
use crate::dll::{self, EffectiveOverride};
use crate::game::GameMods;
use crate::manifest::ModManifest;
use crate::merge::{Owner, merge};
use crate::path::{ModId, ModPath};

/// One slot of an account's load order.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct Slot {
    pub id: ModId,
    pub enabled: bool,
}

/// The paths and DLLs whose winner a move changes; empty when the move only reorders mods that
/// share nothing.
#[derive(Debug, Default)]
#[non_exhaustive]
pub struct OrderChange {
    pub files: Vec<WinnerChange>,
    pub dlls: Vec<DllChange>,
}

impl OrderChange {
    #[must_use]
    #[inline]
    pub const fn is_empty(&self) -> bool {
        self.files.is_empty() && self.dlls.is_empty()
    }
}

#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct WinnerChange {
    /// As the play tree spells it.
    pub path: ModPath,
    pub before: ModId,
    pub after: ModId,
}

/// A DLL whose `WINEDLLOVERRIDES` mode changes, since the mod that wins it asks for another one.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct DllChange {
    pub before: EffectiveOverride,
    pub after: EffectiveOverride,
}

/// A hard ordering edge between two mods: `first` has to load before `then`.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct Constraint {
    pub first: ModId,
    pub then: ModId,
}

/// The hard constraints between `mods`, which the Mods tab flags when the load order breaks one.
/// None yet: provider dependencies and pack edges (mod packs design, section 5.2) plug in here.
#[must_use]
#[inline]
pub const fn constraints(_mods: &[ModManifest]) -> Vec<Constraint> {
    Vec::new()
}

/// `mods` are the enabled ones in their current load order, `moved` the enabled ids in the order
/// the move would leave.
pub(crate) fn changes(game: &GameMods, mut mods: Vec<ModManifest>, moved: &[ModId]) -> Result<OrderChange> {
    let before = Standing::of(game, &mods)?;
    let rank: HashMap<&ModId, usize> = moved.iter().enumerate().map(|(index, id)| (id, index)).collect();
    mods.sort_by_key(|manifest| rank.get(&manifest.id).copied().unwrap_or(usize::MAX));
    Ok(before.against(Standing::of(game, &mods)?))
}

/// The winners that change from the enabled mods `before` to the enabled mods `after`, each in
/// load order; a path or DLL no mod of `before` wins is not a change, since nothing loses it.
pub(crate) fn between(game: &GameMods, before: &[ModManifest], after: &[ModManifest]) -> Result<OrderChange> {
    Ok(Standing::of(game, before)?.against(Standing::of(game, after)?))
}

/// Who wins what among some enabled mods.
struct Standing {
    files: BTreeMap<String, (ModPath, ModId)>,
    dlls: Vec<EffectiveOverride>,
}

impl Standing {
    fn of(game: &GameMods, mods: &[ModManifest]) -> Result<Self> {
        Ok(Self {
            files: winners(game, mods)?,
            dlls: dll::effective(mods),
        })
    }

    fn against(self, after: Self) -> OrderChange {
        let files = self
            .files
            .into_iter()
            .filter_map(|(key, (path, before))| {
                let winner = &after.files.get(&key)?.1;
                (*winner != before).then(|| WinnerChange {
                    path,
                    before,
                    after: winner.clone(),
                })
            })
            .collect();
        let dlls = after
            .dlls
            .into_iter()
            .filter_map(|after| {
                let before = self.dlls.iter().find(|before| before.dll == after.dll)?;
                (before.mode != after.mode).then(|| DllChange {
                    before: before.clone(),
                    after,
                })
            })
            .collect();
        OrderChange { files, dlls }
    }
}

/// Per folded path a mod wins, that mod.
fn winners(game: &GameMods, mods: &[ModManifest]) -> Result<BTreeMap<String, (ModPath, ModId)>> {
    Ok(merge(game, mods)?
        .into_iter()
        .filter_map(|(key, wanted)| {
            let Some(&Owner::Mod(winner)) = wanted.providers.last() else {
                return None;
            };
            Some((key, (wanted.path, mods.get(winner)?.id.clone())))
        })
        .collect())
}
