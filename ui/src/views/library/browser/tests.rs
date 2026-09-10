use super::Sort;
use super::{Slot, restore_slots, save_slots_encode};

fn slot(source: &str, sort: Sort) -> Slot {
    Slot {
        key: 0,
        source: source.to_string(),
        sort,
    }
}

#[test]
fn a_layout_survives_a_round_trip() {
    let saved = vec![slot("favorites", Sort::Title), slot("c1", Sort::MostPlayed)];
    let restored = restore_slots(Some(&save_slots_encode(&saved).unwrap()), &["c1".to_string()]);
    assert!(restored.len() == 2);
    assert!(restored[0].source == "favorites" && restored[0].sort == Sort::Title);
    assert!(restored[1].source == "c1" && restored[1].sort == Sort::MostPlayed);
}

/// Keys are per-mount handles, so restoring has to hand out fresh
/// distinct ones however the saved list looked.
#[test]
fn restored_shelves_get_distinct_keys() {
    let restored = restore_slots(
        Some(&save_slots_encode(&vec![slot("all", Sort::Title); 3]).unwrap()),
        &[],
    );
    assert!(restored.iter().map(|s| s.key).collect::<Vec<_>>() == vec![0, 1, 2]);
}

/// A shelf pointing at a deleted collection goes away; the built-in
/// sources beside it stay.
#[test]
fn a_shelf_on_a_deleted_collection_is_dropped() {
    let saved = vec![slot("installed", Sort::Title), slot("gone", Sort::Title)];
    let restored = restore_slots(Some(&save_slots_encode(&saved).unwrap()), &[]);
    assert!(restored.len() == 1);
    assert!(restored[0].source == "installed");
}

/// Nothing saved, and nothing left after pruning, both open on the
/// whole library rather than on no shelf at all.
#[test]
fn an_empty_layout_falls_back_to_the_whole_library() {
    for raw in [None, Some("not json"), Some("[]")] {
        let restored = restore_slots(raw, &[]);
        assert!(restored.len() == 1);
        assert!(restored[0].source == "all");
    }
}
