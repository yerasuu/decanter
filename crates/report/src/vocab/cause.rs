//! What decanter's own lines append after a colon: the error of the failed call, as the log
//! writes it. Anything not listed here is left out of the report, never kept as text.

use super::{Template, t};

pub const TEMPLATES: &[Template] = &[
    t("cause.db.locked", "database is locked"),
    t("cause.db.open_locked", "database open failed: database is locked"),
    t("cause.db.open_failed", "database open failed{_}"),
    t("cause.process.exit_status", "umu-run exited with exit status: {number}"),
    t("cause.process.rsync_exit", "rsync failed: exit status: {number}: {_}"),
    t("cause.process.exit_status_plain", "exit status: {number}"),
    t("cause.process.build_failed", "build failed"),
    t(
        "cause.json.expected_value",
        "expected value at line {number} column {number}",
    ),
    t(
        "cause.gog.request_expected_value",
        "gog {owned-games|achievements|products|order-history} request failed: expected value at line {number} column {number}",
    ),
    t(
        "cause.gog.session_invalid_expected_value",
        "GOG session is no longer valid: expected value at line {number} column {number}",
    ),
    t("cause.gog.session_not_valid", "GOG session is not valid{_}"),
    t("cause.gog.session_no_longer_valid", "GOG session is no longer valid{_}"),
    t("cause.keyring.no_entry", "No matching entry found in secure storage"),
    t("cause.keyring.platform_failure", "Platform secure storage failure{_}"),
    t("cause.os.not_found", "No such file or directory (os error {number})"),
    t("cause.os.permission_denied", "Permission denied (os error {number})"),
    t("cause.os.no_space", "No space left on device (os error {number})"),
    t("cause.os.error", "{_} (os error {number})"),
    t("cause.net.http_status", "HTTP {number}"),
    t("cause.net.timed_out", "operation timed out"),
    t("cause.net.send_failed", "error sending request for url ({_})"),
];
