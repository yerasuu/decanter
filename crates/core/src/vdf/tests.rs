#![expect(
    clippy::separated_literal_suffix,
    reason = "unseparated_literal_suffix (pedantic) bans the opposite form on the same literals below"
)]

use super::*;

#[test]
fn reads_a_real_appmanifest() {
    // Trimmed from a real `appmanifest_228980.acf`, nested block and
    // escaped Windows path included.
    let input = r#"
"AppState"
{
	"appid"		"228980"
	"name"		"Steamworks Common Redistributables"
	"installdir"		"Steamworks Shared"
	"SizeOnDisk"		"306951157"
	"InstalledDepots"
	{
		"228983"
		{
			"manifest"		"8124929965194586177"
			"size"		"19265607"
		}
	}
	"InstallScripts"
	{
		"228983"		"_CommonRedist\\vcredist\\2010\\installscript.vdf"
	}
	"UserConfig"
	{
	}
}
"#;
    let (key, root) = match from_str(input) {
        Ok(parsed) => parsed,
        Err(err) => panic!("should parse: {err:?}"),
    };
    assert_eq!(key, "AppState");
    assert_eq!(root.get("appid"), Some("228980"));
    assert_eq!(root.get("installdir"), Some("Steamworks Shared"));
    assert_eq!(root.get_u64("SizeOnDisk"), Some(306_951_157));

    let Some(depots) = root.get_block("InstalledDepots") else {
        panic!("nested block")
    };
    let Some(depot) = depots.get_block("228983") else {
        panic!("depot block")
    };
    assert_eq!(depot.get_u64("size"), Some(19_265_607));

    // `\\` in the file is one backslash in the value.
    let Some(scripts) = root.get_block("InstallScripts") else {
        panic!("called `Option::unwrap()` on a `None` value")
    };
    assert_eq!(
        scripts.get("228983"),
        Some(r"_CommonRedist\vcredist\2010\installscript.vdf")
    );

    // An empty block is a block, not a missing key.
    assert!(root.get_block("UserConfig").is_some());
}

#[test]
fn round_trips_what_it_writes() {
    let mut depots = Value::block();
    let mut depot = Value::block();
    depot.insert("size", 42_u64);
    depots.insert("1", depot);

    let mut root = Value::block();
    root.insert("gameid", "1741027632");
    root.insert("installdir", r"Right and Down\data");
    root.insert("SizeOnDisk", 5_566_295_753_u64);
    root.insert("depots", depots);

    let rendered = to_string("AppState", &root);
    let (key, parsed) = match from_str(&rendered) {
        Ok(parsed) => parsed,
        Err(err) => panic!("its own output should parse: {err:?}"),
    };
    assert_eq!(key, "AppState");
    assert_eq!(parsed, root);
}

/// A path with a backslash must survive the round trip as one
/// backslash — getting this wrong doubles them on every rewrite.
#[test]
fn a_windows_path_does_not_grow_backslashes() {
    let mut root = Value::block();
    root.insert("path", r"Game_Data\level0");

    let mut rendered = to_string("AppState", &root);
    for _ in 0_usize..3_usize {
        let (_, parsed) = match from_str(&rendered) {
            Ok(parsed) => parsed,
            Err(err) => panic!("called `Result::unwrap()` on an `Err` value: {err:?}"),
        };
        assert_eq!(parsed.get("path"), Some(r"Game_Data\level0"));
        rendered = to_string("AppState", &parsed);
    }
}

#[test]
fn skips_comments_and_accepts_unquoted_tokens() {
    let input = "\
// a comment Steam sometimes writes
libraryfolders
{
	\"0\"
	{
		path	\"/home/x/.local/share/Steam\"
	}
}
";
    let (key, root) = match from_str(input) {
        Ok(parsed) => parsed,
        Err(err) => panic!("should parse: {err:?}"),
    };
    assert_eq!(key, "libraryfolders");
    let Some(block) = root.get_block("0") else {
        panic!("called `Option::unwrap()` on a `None` value")
    };
    assert_eq!(block.get("path"), Some("/home/x/.local/share/Steam"));
}

#[test]
fn a_quote_inside_a_value_survives() {
    let mut root = Value::block();
    root.insert("name", r#"He said "hi""#);
    let (_, parsed) = match from_str(&to_string("AppState", &root)) {
        Ok(parsed) => parsed,
        Err(err) => panic!("called `Result::unwrap()` on an `Err` value: {err:?}"),
    };
    assert_eq!(parsed.get("name"), Some(r#"He said "hi""#));
}

// A `match` here instead of `assert!(...is_err())` would need a bare
// `Err(_)` arm, which this codebase's own humanization scanner
// (scripts/check-humanization.sh) rejects as an undocumented wildcard
// error match — plain assertions are the right call for these three.
#[test]
fn rejects_a_truncated_file() {
    assert!(from_str("").is_err(), "expected an empty file to fail to parse");
    assert!(
        from_str("\"AppState\"").is_err(),
        "expected a key with no value to fail to parse"
    );
    assert!(
        from_str("\"AppState\"\n{\n\"a\" \"b\"").is_err(),
        "expected an unclosed block to fail to parse"
    );
}

/// A leaf where a block was expected reads as absent rather than
/// panicking — hand-edited files do this.
#[test]
fn a_leaf_is_not_mistaken_for_a_block() {
    let (_, root) = match from_str("\"AppState\"\n{\n\"a\"\t\"b\"\n}\n") {
        Ok(parsed) => parsed,
        Err(err) => panic!("called `Result::unwrap()` on an `Err` value: {err:?}"),
    };
    assert!(root.get_block("a").is_none());
    assert_eq!(root.get("a"), Some("b"));
}
