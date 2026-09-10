use super::*;

// `clippy::panic` (restriction) bans the `panic!()` below; `clippy::expect_used`
// (also restriction, already flagged here) is what it replaces. The two are
// mutually exclusive on a fallible `let-else` in test code, so this keeps
// `panic!()` with the original message rather than reintroducing `.expect()`.

/// `/` always exists and is never full — enough to know the syscall
/// path itself works, since the exact numbers aren't something a test
/// can pin down.
#[test]
fn root_filesystem_reports_plausible_numbers() {
    let Some((free, total)) = disk_usage(Path::new("/")) else {
        panic!("statvfs on / should succeed");
    };
    assert!(total > 0);
    assert!(free <= total);
}

#[test]
fn a_path_that_does_not_exist_reports_nothing() {
    assert_eq!(disk_usage(Path::new("/this/does/not/exist/at/all")), None);
}
