use super::*;

/// Trimmed from the real build manifest for The Coma 2: Vicious
/// Sisters — a game whose build carries a depot for the base game and
/// one for each of its DLC, with the base game's *not* first.
const BUILD_META: &str = r#"{
    "installDirectory": "The Coma 2 Vicious Sisters",
    "depots": [
        {"productId": "1191925155", "osBitness": ["64"], "manifest": "37baaf1d", "size": 10682075},
        {"productId": "1582311007", "osBitness": ["64"], "manifest": "1603c1e1", "size": 11932510},
        {"productId": "1854689188", "osBitness": ["64"], "manifest": "0faba0b7", "size": 2422808425},
        {"productId": "1191925155", "osBitness": ["64"], "manifest": "123b337d", "size": 639},
        {"productId": "1854689188", "osBitness": ["64"], "manifest": "bb44d3c8", "size": 6497}
    ]
}"#;

fn meta() -> BuildMeta {
    serde_json::from_str(BUILD_META).expect("build meta should parse")
}

/// The bug this guards: taking the first Windows-compatible depot
/// installed a DLC's 10 MB instead of the game's 2.4 GB, and then
/// 404'd because the chunks were signed for the wrong product.
#[test]
fn takes_every_depot_of_the_requested_product_and_no_others() {
    let meta = meta();
    let base: Vec<&str> = select_depots(&meta, 1854689188)
        .iter()
        .map(|depot| depot.manifest.as_str())
        .collect();
    assert_eq!(base, vec!["0faba0b7", "bb44d3c8"]);
}

/// A DLC's content lives in its parent's build, tagged with the DLC's
/// own product id — which is how installing one is possible at all.
#[test]
fn a_dlc_gets_only_its_own_depots_out_of_the_parents_build() {
    let meta = meta();
    let dlc: Vec<&str> = select_depots(&meta, 1191925155)
        .iter()
        .map(|depot| depot.manifest.as_str())
        .collect();
    assert_eq!(dlc, vec!["37baaf1d", "123b337d"]);
}

#[test]
fn a_product_with_no_depot_in_this_build_selects_nothing() {
    assert!(select_depots(&meta(), 999999999).is_empty());
}

/// A depot for another OS is not this product's content even when the
/// product id matches.
#[test]
fn skips_depots_for_other_platforms() {
    let meta: BuildMeta = serde_json::from_str(
        r#"{"installDirectory": "X", "depots": [
             {"productId": "42", "osBitness": [], "manifest": "mac"},
             {"productId": "42", "osBitness": ["64"], "manifest": "win"}
           ]}"#,
    )
    .unwrap();
    let picked: Vec<&str> = select_depots(&meta, 42)
        .iter()
        .map(|depot| depot.manifest.as_str())
        .collect();
    assert_eq!(picked, vec!["win"]);
}

/// Mainline over any named branch, and something rather than nothing
/// when every build is on one.
#[test]
fn picks_the_mainline_build() {
    let builds: Vec<Build> = serde_json::from_str(
        r#"[{"build_id":"1","link":"a","branch":"beta"},
            {"build_id":"2","link":"b","branch":null}]"#,
    )
    .unwrap();
    assert_eq!(select_build(&builds).unwrap().build_id, "2");

    let only_branches: Vec<Build> = serde_json::from_str(r#"[{"build_id":"1","link":"a","branch":"beta"}]"#).unwrap();
    assert_eq!(select_build(&only_branches).unwrap().build_id, "1");
    assert!(select_build(&[]).is_none());
}
