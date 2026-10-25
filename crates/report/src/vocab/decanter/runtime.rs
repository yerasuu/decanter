//! Warnings and errors of decanter-runtime: the literal message of each `tracing::warn!` or
//! `tracing::error!`; the log appends ` job=`, ` exit=` and `: <error>` after it.

use super::super::{Template, t};

#[rustfmt::skip]
pub const TEMPLATES: &[Template] = &[
    t("decanter.runtime.launch.failed_to_wait_for_umu", "failed to wait for umu-run"),
    t("decanter.runtime.launch.umu_run_exited_with_a", "umu-run exited with a failure status"),
    t("decanter.runtime.launch.failed_to_spawn_umu_run", "failed to spawn umu-run"),
    t("decanter.runtime.launch.rejecting_launch_no_such_executable", "rejecting launch: no such executable on disk"),
    t("decanter.runtime.macos.spawned_pid_cannot_name_a", "spawned pid cannot name a process group to signal; the session is not followed"),
    t("decanter.runtime.macos.cannot_check_the_session_s", "cannot check the session's process group; ending the session"),
    t("decanter.runtime.mod.failed_to_wait_for_the", "failed to wait for the game"),
    t("decanter.runtime.mod.game_ended_by_decanter_killing", "game ended by decanter killing it; its exit code is not the game's own"),
    t("decanter.runtime.mod.game_exited_with_a_failure", "game exited with a failure status"),
    t("decanter.runtime.mod.failed_to_spawn_the_game", "failed to spawn the game"),
    t("decanter.runtime.mod.session_did_not_close_by", "session did not close by itself; killing it"),
    t("decanter.runtime.mod.session_still_running_after_being", "session still running after being ended"),
    t("decanter.runtime.windows.cannot_list_the_session_s", "cannot list the session's processes"),
    t("decanter.runtime.windows.cannot_list_the_session_s_2", "cannot list the session's processes; ending the session"),
    t("decanter.runtime.windows.cannot_make_the_session_s", "cannot make the session's job end its processes; relying on taskkill"),
    t("decanter.runtime.windows.the_game_has_no_process", "the game has no process handle; following the launched process alone"),
    t("decanter.runtime.windows.cannot_create_a_job_object", "cannot create a job object; following the launched process alone"),
    t("decanter.runtime.windows.cannot_assign_the_game_to", "cannot assign the game to its job object; following the launched process alone"),
    t("decanter.runtime.path.rejecting_path_climbs_out_of", "rejecting path: climbs out of or escapes its base directory"),
    t("decanter.runtime.path.rejecting_path_sanitizes_to_nothing", "rejecting path: sanitizes to nothing"),
    t("decanter.runtime.prefix.rejecting_windows_path_not_on", "rejecting windows path: not on the C: drive"),
    t("decanter.runtime.prefix.rejecting_windows_path_escapes_the", "rejecting windows path: escapes the prefix"),
    t("decanter.runtime.prefix.pid_out_of_range_for", "pid out of range for a signal, skipping"),
    t("decanter.runtime.prefix.proc_unreadable_reporting_no_live", "/proc unreadable, reporting no live processes for any prefix"),
    t("decanter.runtime.proton.neither_xdg_data_home_nor", "neither XDG_DATA_HOME nor HOME is set, cannot resolve the Steam compat dir"),
    t("decanter.runtime.umu.cannot_remove_an_old_umu", "cannot remove an old umu-run"),
];
