use super::find_primary_executable_heuristic;

/// Reproduces a real GOG install (Betrayer, a UDK-era game with no
/// `goggame-*.info`): the executable lives several folders below the
/// install root, so a top-level-only search never finds it.
#[test]
fn finds_an_executable_named_after_the_install_directory_at_any_depth() {
    let paths = vec![
        "unins000.exe".to_string(),
        "Binaries/Win32/Betrayer.exe".to_string(),
        "Binaries/Redist/UE3Redist.exe".to_string(),
    ];
    assert_eq!(
        find_primary_executable_heuristic(&paths, "Betrayer"),
        Some("Binaries/Win32/Betrayer.exe".to_string())
    );
}

#[test]
fn falls_back_to_a_single_unambiguous_top_level_exe() {
    let paths = vec!["unins000.exe".to_string(), "GameLauncher.exe".to_string()];
    assert_eq!(
        find_primary_executable_heuristic(&paths, "Some Other Name"),
        Some("GameLauncher.exe".to_string())
    );
}

#[test]
fn refuses_to_guess_between_two_top_level_candidates() {
    let paths = vec!["Game32.exe".to_string(), "Game64.exe".to_string()];
    assert_eq!(find_primary_executable_heuristic(&paths, "Some Other Name"), None);
}

#[test]
fn ignores_a_nested_non_matching_exe_when_no_top_level_candidate_exists() {
    let paths = vec!["unins000.exe".to_string(), "Tools/Editor.exe".to_string()];
    assert_eq!(find_primary_executable_heuristic(&paths, "Some Other Name"), None);
}
