// SPDX-License-Identifier: GPL-3.0-or-later
//! The one sequence a pack installs and loads in (design, section 5.2): its entries' `order`, moved
//! only as far as their edges demand, the same on every machine.

use core::cmp::Reverse;
use std::collections::{BTreeMap, BinaryHeap, HashMap, VecDeque};

use crate::{Entry, EntryKey, Pack};

/// A dependency a provider declares between two entries of the pack (a Thunderstore manifest's,
/// mod.io's, or a Nexus requirement), handed in by the app, which alone talks to providers.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct Dependency {
    pub dependant: EntryKey,
    pub required: EntryKey,
    pub strength: Strength,
}

impl Dependency {
    #[must_use]
    #[inline]
    pub const fn new(dependant: EntryKey, required: EntryKey, strength: Strength) -> Self {
        Self {
            dependant,
            required,
            strength,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Strength {
    /// Thunderstore's and mod.io's: installed first, and the dependant stays off without it.
    Hard,
    /// Nexus' requirements: ordering only, and the first edge given up to break a cycle.
    Advisory,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum EdgeKind {
    /// The pack's `requires`, a provider's dependency materialised into it included.
    Requires,
    /// The pack's `after`.
    After,
    /// A provider's hard dependency the pack does not list.
    Provider,
    /// A provider's advisory requirement.
    Suggested,
}

/// `before` is placed ahead of `after`.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Edge {
    pub before: EntryKey,
    pub after: EntryKey,
    pub kind: EdgeKind,
}

/// Entries that each wait for another of them, with an edge someone wrote among them, so the pack
/// can't be applied until the user changes one.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct Cycle {
    /// In the pack's order.
    pub entries: Vec<EntryKey>,
    /// Every edge among `entries`.
    pub edges: Vec<Edge>,
}

/// An entry the edges pulled ahead of its `order`, to just before `before`, its earliest dependant.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Move {
    pub key: EntryKey,
    /// 0-based positions in the pack's order and in the resolved sequence.
    pub from: usize,
    pub to: usize,
    pub before: EntryKey,
}

/// A hard edge the stored `order` breaks: the pack editor refuses a drop that makes one, and a
/// hand-edited file that has one is applied in the resolved sequence instead.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Contradiction {
    pub dependant: EntryKey,
    pub required: EntryKey,
    pub kind: EdgeKind,
}

#[derive(Debug, Clone, thiserror::Error)]
#[non_exhaustive]
pub enum ResolveError {
    #[error("{} wait for each other", .0.entries.iter().map(EntryKey::as_str).collect::<Vec<_>>().join(", "))]
    Cycle(Cycle),
    #[error("{0} is not an entry of the pack")]
    UnknownEntry(EntryKey),
    #[error("two entries of the pack are {0}")]
    DuplicateEntry(EntryKey),
}

#[derive(Debug, Clone)]
pub struct Resolution {
    sequence: Vec<EntryKey>,
    /// Per position in `sequence`, the earlier positions it hard-requires.
    requirements: Vec<Vec<usize>>,
    moves: Vec<Move>,
    contradictions: Vec<Contradiction>,
    dropped: Vec<Edge>,
}

impl Resolution {
    /// The install sequence; the load order is this, restricted to the entries that end enabled.
    #[must_use]
    #[inline]
    pub fn sequence(&self) -> &[EntryKey] {
        &self.sequence
    }

    /// What *Reordered by dependencies: N entries moved* lists, in resolved order.
    #[must_use]
    #[inline]
    pub fn moves(&self) -> &[Move] {
        &self.moves
    }

    #[must_use]
    #[inline]
    pub fn contradictions(&self) -> &[Contradiction] {
        &self.contradictions
    }

    /// Provider edges given up to break cycles made only of them, for the log and the editor.
    #[must_use]
    #[inline]
    pub fn dropped(&self) -> &[Edge] {
        &self.dropped
    }

    pub(crate) fn requirements(&self) -> &[Vec<usize>] {
        &self.requirements
    }

    /// Writes the resolved sequence back as the entries' `order`, `10` apart, when edges moved any
    /// entry: what saving does after a reorder (design, section 5.2). `false` when nothing moved.
    #[inline]
    pub fn write_order(&self, pack: &mut Pack) -> bool {
        if self.moves.is_empty() {
            return false;
        }
        let positions: HashMap<&EntryKey, usize> = self
            .sequence
            .iter()
            .enumerate()
            .map(|(index, key)| (key, index))
            .collect();
        for entry in &mut pack.entries {
            if let Some(&index) = positions.get(&entry.key) {
                let slot = u32::try_from(index).unwrap_or(u32::MAX).saturating_add(1);
                entry.order = slot.saturating_mul(10);
            }
        }
        true
    }
}

#[derive(Debug, Clone, Copy, Default)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "one flag per source of an edge, any of which may name the same pair"
)]
struct Link {
    requires: bool,
    after: bool,
    provider: bool,
    suggested: bool,
}

impl Link {
    const fn is_hard(self) -> bool {
        self.requires || self.provider
    }

    /// Written by the pack's author rather than taken from a provider: a `requires` that matches a
    /// provider's dependency was materialised from it, so it counts as the provider's.
    const fn is_user(self) -> bool {
        self.after || (self.requires && !self.provider)
    }

    const fn kind(self) -> EdgeKind {
        if self.requires {
            EdgeKind::Requires
        } else if self.after {
            EdgeKind::After
        } else if self.provider {
            EdgeKind::Provider
        } else {
            EdgeKind::Suggested
        }
    }
}

/// Edges by `(before, after)`, both as positions in the pack's order.
type Links = BTreeMap<(usize, usize), Link>;

/// The pack's entries in one stable topological order.
///
/// Kahn's algorithm with a priority queue: of the entries whose predecessors are all placed, the
/// one taken next is the one with the earliest `order` among itself and everything that depends on
/// it, then its own `order`, then its key. Without edges that is the pack's order; with them a
/// dependency moves up to just before its earliest dependant instead of its dependants sinking.
/// A cycle made only of provider edges is broken by giving up the edge whose dependant comes last
/// (advisory ones first); a cycle with an edge from the pack refuses.
///
/// # Errors
///
/// [`ResolveError::Cycle`] with the entries and edges of the first refused cycle;
/// [`ResolveError::UnknownEntry`] and [`ResolveError::DuplicateEntry`] for a pack or dependency
/// list that does not hold together, which a validated pack never is.
#[inline]
pub fn resolve(pack: &Pack, provider: &[Dependency]) -> Result<Resolution, ResolveError> {
    let mut ranked: Vec<&Entry> = pack.entries.iter().collect();
    ranked.sort_by(|left, right| left.order.cmp(&right.order).then_with(|| left.key.cmp(&right.key)));
    let mut links = collect_links(&ranked, provider)?;
    let (topological, dropped) = break_cycles(&ranked, &mut links)?;
    let key = |index: usize| key_of(&ranked, index);

    let earliest = earliest_dependant(ranked.len(), &links, &topological);
    let sequence = prioritised(ranked.len(), &links, &earliest);
    let mut position = vec![0_usize; ranked.len()];
    for (index, &node) in sequence.iter().enumerate() {
        if let Some(slot) = position.get_mut(node) {
            *slot = index;
        }
    }
    let at = |node: usize| position.get(node).copied().unwrap_or_default();
    let mut moves: Vec<Move> = earliest
        .iter()
        .enumerate()
        .filter(|&(node, &first)| first < node)
        .filter_map(|(node, &first)| {
            Some(Move {
                key: key(node)?,
                from: node,
                to: at(node),
                before: key(first)?,
            })
        })
        .collect();
    moves.sort_by_key(|moved| moved.to);
    let contradictions = links
        .iter()
        .filter(|&(&(before, after), link)| link.is_hard() && before > after)
        .filter_map(|(&(before, after), link)| {
            Some(Contradiction {
                dependant: key(after)?,
                required: key(before)?,
                kind: if link.requires {
                    EdgeKind::Requires
                } else {
                    EdgeKind::Provider
                },
            })
        })
        .collect();
    let mut requirements = vec![Vec::new(); ranked.len()];
    for (&(before, after), link) in &links {
        if link.is_hard()
            && let Some(list) = requirements.get_mut(at(after))
        {
            list.push(at(before));
        }
    }
    Ok(Resolution {
        sequence: sequence.into_iter().filter_map(key).collect(),
        requirements,
        moves,
        contradictions,
        dropped,
    })
}

/// The pack's and the providers' edges, merged per pair of entries.
fn collect_links(ranked: &[&Entry], provider: &[Dependency]) -> Result<Links, ResolveError> {
    let mut rank = HashMap::with_capacity(ranked.len());
    for (index, entry) in ranked.iter().enumerate() {
        if rank.insert(&entry.key, index).is_some() {
            return Err(ResolveError::DuplicateEntry(entry.key.clone()));
        }
    }
    let find = |key: &EntryKey| {
        rank.get(key)
            .copied()
            .ok_or_else(|| ResolveError::UnknownEntry(key.clone()))
    };
    let mut links = Links::new();
    for (index, entry) in ranked.iter().enumerate() {
        for key in &entry.requires {
            let before = find(key)?;
            if before != index {
                links.entry((before, index)).or_default().requires = true;
            }
        }
        for key in &entry.after {
            let before = find(key)?;
            if before != index {
                links.entry((before, index)).or_default().after = true;
            }
        }
    }
    for dependency in provider {
        let (before, after) = (find(&dependency.required)?, find(&dependency.dependant)?);
        if before != after {
            let link = links.entry((before, after)).or_default();
            match dependency.strength {
                Strength::Hard => link.provider = true,
                Strength::Advisory => link.suggested = true,
            }
        }
    }
    Ok(links)
}

/// A topological order of the entries, and the provider edges given up to reach it.
fn break_cycles(ranked: &[&Entry], links: &mut Links) -> Result<(Vec<usize>, Vec<Edge>), ResolveError> {
    let mut dropped = Vec::new();
    loop {
        let stuck = match topological(ranked.len(), links) {
            Ok(order) => return Ok((order, dropped)),
            Err(stuck) => stuck,
        };
        let members = cycle_members(ranked.len(), links, &stuck);
        let inside: Vec<((usize, usize), Link)> = links
            .iter()
            .filter(|((before, after), _)| members.contains(before) && members.contains(after))
            .map(|(&pair, &link)| (pair, link))
            .collect();
        let last = |candidates: &mut dyn Iterator<Item = &((usize, usize), Link)>| {
            candidates
                .max_by_key(|((before, after), _)| (*after, *before))
                .map(|(pair, _)| *pair)
        };
        let advisory = last(&mut inside.iter().filter(|(_, link)| !link.is_user() && !link.is_hard()));
        let give_up = advisory.or_else(|| {
            inside
                .iter()
                .all(|(_, link)| !link.is_user())
                .then(|| last(&mut inside.iter()))
                .flatten()
        });
        let Some(pair) = give_up else {
            return Err(ResolveError::Cycle(Cycle {
                entries: members.iter().filter_map(|&index| key_of(ranked, index)).collect(),
                edges: inside
                    .into_iter()
                    .filter_map(|(pair, link)| edge(ranked, pair, link))
                    .collect(),
            }));
        };
        if let Some(link) = links.remove(&pair) {
            dropped.extend(edge(ranked, pair, link));
        }
    }
}

fn key_of(ranked: &[&Entry], index: usize) -> Option<EntryKey> {
    ranked.get(index).map(|entry| entry.key.clone())
}

fn edge(ranked: &[&Entry], (before, after): (usize, usize), link: Link) -> Option<Edge> {
    Some(Edge {
        before: key_of(ranked, before)?,
        after: key_of(ranked, after)?,
        kind: link.kind(),
    })
}

fn successors(count: usize, links: &Links) -> Vec<Vec<usize>> {
    let mut next = vec![Vec::new(); count];
    for &(before, after) in links.keys() {
        if let Some(list) = next.get_mut(before) {
            list.push(after);
        }
    }
    next
}

/// Kahn's pass in any order; `Err` with the entries it could not place, which hold a cycle.
fn topological(count: usize, links: &Links) -> Result<Vec<usize>, Vec<usize>> {
    let next = successors(count, links);
    let mut waiting = vec![0_usize; count];
    for &(_, after) in links.keys() {
        if let Some(slot) = waiting.get_mut(after) {
            *slot = slot.saturating_add(1);
        }
    }
    let mut ready: VecDeque<usize> = (0..count).filter(|&node| waiting.get(node) == Some(&0)).collect();
    let mut order = Vec::with_capacity(count);
    while let Some(node) = ready.pop_front() {
        order.push(node);
        for &after in next.get(node).into_iter().flatten() {
            if let Some(slot) = waiting.get_mut(after) {
                *slot = slot.saturating_sub(1);
                if *slot == 0 {
                    ready.push_back(after);
                }
            }
        }
    }
    if order.len() == count {
        Ok(order)
    } else {
        Err((0..count)
            .filter(|&node| waiting.get(node).is_some_and(|&left| left > 0))
            .collect())
    }
}

/// The strongly connected component of the earliest entry of `stuck` that is on a cycle, in the
/// pack's order. Reachability per entry is quadratic, which the entry cap keeps small.
fn cycle_members(count: usize, links: &Links, stuck: &[usize]) -> Vec<usize> {
    let next = successors(count, links);
    let mut inside = vec![false; count];
    for &node in stuck {
        if let Some(slot) = inside.get_mut(node) {
            *slot = true;
        }
    }
    let reach = |from: usize| {
        let mut seen = vec![false; count];
        let mut queue = VecDeque::from([from]);
        while let Some(node) = queue.pop_front() {
            for &after in next.get(node).into_iter().flatten() {
                if inside.get(after) == Some(&true) && seen.get(after) == Some(&false) {
                    if let Some(slot) = seen.get_mut(after) {
                        *slot = true;
                    }
                    queue.push_back(after);
                }
            }
        }
        seen
    };
    for &node in stuck {
        let from_node = reach(node);
        if from_node.get(node) != Some(&true) {
            continue;
        }
        return stuck
            .iter()
            .copied()
            .filter(|&other| {
                other == node || (from_node.get(other) == Some(&true) && reach(other).get(node) == Some(&true))
            })
            .collect();
    }
    // Kahn leaves entries unplaced only when one of them is on a cycle, so this is never reached;
    // reporting them all keeps the caller's loop finite if it were.
    stuck.to_vec()
}

/// Per entry, the earliest position among itself and everything that depends on it.
fn earliest_dependant(count: usize, links: &Links, topological: &[usize]) -> Vec<usize> {
    let next = successors(count, links);
    let mut earliest: Vec<usize> = (0..count).collect();
    for &node in topological.iter().rev() {
        let first = next
            .get(node)
            .into_iter()
            .flatten()
            .filter_map(|&after| earliest.get(after).copied())
            .fold(node, usize::min);
        if let Some(slot) = earliest.get_mut(node) {
            *slot = first;
        }
    }
    earliest
}

fn prioritised(count: usize, links: &Links, earliest: &[usize]) -> Vec<usize> {
    let next = successors(count, links);
    let mut waiting = vec![0_usize; count];
    for &(_, after) in links.keys() {
        if let Some(slot) = waiting.get_mut(after) {
            *slot = slot.saturating_add(1);
        }
    }
    let priority = |node: usize| Reverse((earliest.get(node).copied().unwrap_or(node), node));
    let mut ready: BinaryHeap<_> = (0..count)
        .filter(|&node| waiting.get(node) == Some(&0))
        .map(priority)
        .collect();
    let mut sequence = Vec::with_capacity(count);
    while let Some(Reverse((_, node))) = ready.pop() {
        sequence.push(node);
        for &after in next.get(node).into_iter().flatten() {
            if let Some(slot) = waiting.get_mut(after) {
                *slot = slot.saturating_sub(1);
                if *slot == 0 {
                    ready.push(priority(after));
                }
            }
        }
    }
    sequence
}
