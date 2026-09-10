use super::*;

fn vars() -> HashMap<&'static str, PathBuf> {
    HashMap::from([
        ("INSTALL", PathBuf::from("/lib/common/Hollow Knight")),
        (
            "APPLICATION_DATA_LOCAL",
            PathBuf::from("/prefix/drive_c/users/steamuser/AppData/Local"),
        ),
    ])
}

#[test]
fn resolves_a_known_variable_with_a_forward_slash_relative_path() {
    assert_eq!(
        resolve("<?APPLICATION_DATA_LOCAL?>/GOG.com/Galaxy/Storage", &vars()),
        Some(PathBuf::from(
            "/prefix/drive_c/users/steamuser/AppData/Local/GOG.com/Galaxy/Storage"
        ))
    );
}

#[test]
fn normalizes_backslashes_in_the_relative_part() {
    assert_eq!(
        resolve(r"<?INSTALL?>\Saves", &vars()),
        Some(PathBuf::from("/lib/common/Hollow Knight/Saves"))
    );
}

#[test]
fn drops_locations_with_an_unknown_variable() {
    assert_eq!(resolve("<?XDG_CONFIG_HOME?>/unity3d/Team Cherry", &vars()), None);
}

#[test]
fn drops_a_template_with_no_marker() {
    assert_eq!(resolve("not-a-template", &vars()), None);
}
