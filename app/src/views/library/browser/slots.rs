use super::Slot;
use crate::views::library::data;
use crate::views::library::filters::Sort;

/// Sources that exist no matter what the user has made, so a saved shelf
/// pointing at one is always valid.
const BUILT_IN_SOURCES: [&str; 4] = ["all", "favorites", "uncategorized", "installed"];

pub(super) fn load_slots(profile_id: &str) -> Vec<Slot> {
    let collection_ids: Vec<String> = data::load_collections(profile_id)
        .list
        .into_iter()
        .map(|collection| collection.id)
        .collect();
    restore_slots(
        data::view_state(profile_id, data::BROWSER_SHELVES).as_deref(),
        &collection_ids,
    )
}

/// The saved layout, with any shelf whose collection has since been
/// deleted dropped: a stale source renders as a permanently empty shelf
/// whose own name is missing from the picker, with no way to tell what it
/// was meant to be.
///
/// Unreadable JSON (hand-edited, or written by a much newer build) is
/// treated as nothing saved, the same as a first run.
fn restore_slots(raw: Option<&str>, collection_ids: &[String]) -> Vec<Slot> {
    let mut slots: Vec<Slot> = raw
        .and_then(|raw| {
            serde_json::from_str::<Vec<Slot>>(raw)
                .inspect_err(|err| tracing::warn!(error = %err, "failed to parse saved shelf layout, starting fresh"))
                .ok()
        })
        .unwrap_or_default()
        .into_iter()
        .filter(|slot| {
            BUILT_IN_SOURCES.contains(&slot.source.as_str()) || collection_ids.iter().any(|id| id == &slot.source)
        })
        .collect();
    // Opens on the whole library: it's the one source that always exists,
    // where "favorites" is absent until something is starred and would
    // leave the first shelf pointing at nothing.
    if slots.is_empty() {
        slots.push(Slot {
            key: 0,
            source: "all".to_string(),
            sort: Sort::RecentlyPlayed,
        });
    }
    // Keys are handed out here, not restored: they only have to be unique
    // within this load, and `next_key` continues from the count.
    for (index, slot) in slots.iter_mut().enumerate() {
        slot.key = index as u32;
    }
    slots
}

pub(super) fn save_slots(profile_id: &str, slots: &[Slot]) {
    if let Some(encoded) = save_slots_encode(slots) {
        data::save_view_state(profile_id, data::BROWSER_SHELVES, &encoded);
    }
}

fn save_slots_encode(slots: &[Slot]) -> Option<String> {
    serde_json::to_string(slots)
        .inspect_err(|err| tracing::warn!(error = %err, "failed to encode shelf layout, not saving"))
        .ok()
}
