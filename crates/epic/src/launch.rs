// SPDX-License-Identifier: GPL-3.0-or-later
//! The command line an Epic game is started with offline (spec, 8.2 and 8.5): no login, no
//! version check, an empty exchange code and no ownership token, so nothing secret is on it.
//! The online launch, which mints an exchange code for `-AUTH_PASSWORD`, is not built: how long
//! that code lives and whether games under Proton take it are still open (spec, Q18 and Q20).

/// Who and what an offline launch names to the game.
#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
pub struct OfflineLaunch<'launch> {
    pub app_name: &'launch str,
    /// The title's `namespace`, its sandbox id.
    pub namespace: &'launch str,
    pub account_id: &'launch str,
    pub display_name: &'launch str,
    /// A bare language code (`es`), as the launcher sends the process language.
    pub language: &'launch str,
}

impl<'launch> OfflineLaunch<'launch> {
    #[inline]
    #[must_use]
    pub const fn new(
        app_name: &'launch str,
        namespace: &'launch str,
        account_id: &'launch str,
        display_name: &'launch str,
        language: &'launch str,
    ) -> Self {
        Self {
            app_name,
            namespace,
            account_id,
            display_name,
            language,
        }
    }
}

/// The arguments after the program, in the order of spec 8.2.
///
/// The manifest's `launch_command`, the catalog's additional command line, then the launcher's
/// own. No user arguments sit between them, since decanter has none to add, and no
/// `-epicdeploymentid`, since the sidecar that holds it (spec, 4.7) is not fetched.
///
/// Both game parts are split on whitespace with quotes kept as they are, the spec's "without POSIX
/// quote handling"; it says nothing of how a quoted space would split.
#[inline]
#[must_use]
pub fn offline_arguments(game: &OfflineLaunch<'_>, launch_command: &str, additional_command_line: &str) -> Vec<String> {
    let launcher = [
        "-AUTH_LOGIN=unused".to_owned(),
        "-AUTH_PASSWORD=".to_owned(),
        "-AUTH_TYPE=exchangecode".to_owned(),
        format!("-epicapp={}", game.app_name),
        "-epicenv=Prod".to_owned(),
        "-EpicPortal".to_owned(),
        format!("-epicusername={}", without_controls(game.display_name)),
        format!("-epicuserid={}", game.account_id),
        format!("-epiclocale={}", game.language),
        format!("-epicsandboxid={}", game.namespace),
    ];
    launch_command
        .split_whitespace()
        .chain(additional_command_line.split_whitespace())
        .map(str::to_owned)
        .chain(launcher)
        .collect()
}

fn without_controls(text: &str) -> String {
    text.chars().filter(|c| !c.is_control()).collect()
}
