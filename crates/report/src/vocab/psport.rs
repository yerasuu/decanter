//! What psport prints when a run stops, fails or faults: `STOP:` and `ERROR:` lines of
//! `src/` and `gpu/games/`, its fault lines (`src/main/signals.c`), its call summary
//! (`src/runtime/runtime_calls.c`) and the GPU library's `<Error>`/`<Critical>` log lines.
//!
//! Order matters: the first template that matches a line wins, so specific entries come before
//! the catch-all of their family.

use super::{Template, t};

/// A GPU log line is `GPU [<class>] <<level>> <file>:<line> <function>: <body>`; the file and the
/// line are discarded, since the function names the place and the line moves with every commit.
macro_rules! report_gpu_line {
    ($body:literal) => {
        concat!("GPU [{class}] <{Error|Critical}> {_}:{_} ", $body)
    };
}

pub const TEMPLATES: &[Template] = &[
    t(
        "psport.fault.guest",
        "Guest fault (signal {number}) at guest offset {offset}, address {_}",
    ),
    t(
        "psport.fault.host",
        "Host fault (signal {number}) in {image}+{offset} ({call}), address {_}",
    ),
    t(
        "psport.fault.host_image",
        "Host fault (signal {number}) in {image}+{offset}{_}",
    ),
    t("psport.fault.rip", "Fault (signal {number}) at RIP {_}"),
    t(
        "psport.calls.summary",
        "{number} unimplemented, {number} stub, {number} failed, {number} status",
    ),
    t(
        "psport.runtime.unsupported_import",
        "Runtime: unsupported import {call} ({_}) returns 0; first argument: {_}",
    ),
    t(
        "psport.runtime.unsupported_import_unwritten",
        "Runtime: unsupported import {call} ({_}) returns {offset} but leaves {_}",
    ),
    t(
        "psport.runtime.import_stub",
        "Runtime: import {call} ({_}) is a stub: it succeeds without doing anything",
    ),
    t(
        "psport.runtime.import_missing",
        "Runtime: import {call} ({_}) has no implementation{_}",
    ),
    t(
        "psport.stop.first_unsupported_import",
        "STOP: first unsupported PS4 import: {call} (index {number})",
    ),
    t("psport.stop.module_ref_overflow", "STOP: module reference overflow"),
    t("psport.stop.watchdog", "STOP: watchdog timeout; thread stacks:"),
    t(
        "psport.stop.unsupported_system_parameter",
        "STOP: unsupported system parameter {code}",
    ),
    t(
        "psport.stop.unsupported_tls_offset",
        "STOP: unsupported TLS module/offset",
    ),
    t("psport.stop.unsupported_tls_module", "STOP: unsupported TLS module id"),
    t(
        "psport.stop.unmapped_rwlock_error",
        "STOP: unmapped rwlock error {code}",
    ),
    t(
        "psport.stop.unmapped_pthread_error",
        "STOP: unmapped host pthread error {code}",
    ),
    t(
        "psport.stop.semaphore_options",
        "STOP: semaphore options are not implemented",
    ),
    t("psport.stop.semaphore_host_error", "STOP: semaphore host error {code}"),
    t(
        "psport.stop.recursive_static_init",
        "STOP: recursive/concurrent static initialization is not supported yet",
    ),
    t(
        "psport.stop.pure_virtual_json",
        "STOP: pure virtual sce::Json::MemAllocator function called",
    ),
    t(
        "psport.stop.pthread_exit_host",
        "STOP: pthread_exit on host-owned/main thread",
    ),
    t(
        "psport.stop.appcontent_service_label",
        "STOP: nonzero AppContent service label unsupported",
    ),
    t(
        "psport.stop.pthread_join_failed",
        "STOP: host pthread_join failed: {code}",
    ),
    t(
        "psport.stop.pthread_create_failed",
        "STOP: host pthread_create failed: {code}",
    ),
    t(
        "psport.stop.stack_protector",
        "STOP: guest stack protector detected corruption",
    ),
    t(
        "psport.stop.signal_no_handler",
        "STOP: guest sent itself signal {number} with no handler; its default action ends the process",
    ),
    t(
        "psport.stop.debug_exception",
        "STOP: guest raised debug exception code={offset} argument={_}",
    ),
    t(
        "psport.stop.event_flag_host_error",
        "STOP: event flag host error {code}",
    ),
    t(
        "psport.stop.called_from_image",
        "STOP: called from {image}+{offset}; game code up the stack:",
    ),
    t(
        "psport.stop.called_from_host",
        "STOP: called from host {_}; game code up the stack:",
    ),
    t("psport.stop.arch_prctl", "STOP: arch_prctl(ARCH_SET_GS){_}"),
    t(
        "psport.stop.aio_create_failed",
        "STOP: aio worker pthread_create failed: {code}",
    ),
    t("psport.stop.aio_wait_error", "STOP: aio wait host error {code}"),
    t("psport.stop.aio_delete_error", "STOP: aio delete host error {code}"),
    t(
        "psport.stop.gpu_assertion",
        "STOP: GPU library assertion failed (see GPU log above)",
    ),
    t("psport.stop.gpu_hook_queue", "STOP: GPU hook queue overflow"),
    t(
        "psport.stop.gpu_device_lost",
        "STOP: GPU device lost ({a Vulkan call|acquiring a swapchain image|binding guest memory|creating the swapchain|presenting|reading the GPU progress|releasing a failed guest memory bind|submit|waiting for a frame|waiting for a submission|waiting for the GPU})",
    ),
    t(
        "psport.stop.appcontent_before_init",
        "STOP: AppContent query before initialization",
    ),
    t(
        "psport.stop.appcontent_needs_app0",
        "STOP: AppContent needs --app0 (its param.sfo)",
    ),
    t(
        "psport.stop.appcontent_before_load",
        "STOP: AppContent called before module load",
    ),
    t(
        "psport.stop.ajm_codec",
        "STOP: Ajm codec {number} is not implemented (0=MP3, 1=ATRAC9, 2=AAC are)",
    ),
    t(
        "psport.error.cpu_lacks",
        "ERROR: this CPU lacks {_}. The game's code was compiled for the PS4's CPU{_}",
    ),
    t(
        "psport.error.no_tls_slot",
        "ERROR: no thread-local slot for the guest's thread pointer: {_}",
    ),
    t("psport.error.content_profile", "ERROR: invalid or late content profile"),
    t(
        "psport.error.address_range",
        "ERROR: cannot reserve the guest's address range: {_}",
    ),
    t("psport.error.cannot_read", "ERROR: cannot read {_}"),
    t(
        "psport.error.unsupported_game_files",
        "ERROR: Unsupported game files: {_} Set PS_SKIP_GAME_CHECK=1 to start anyway.",
    ),
    t(
        "psport.error.small_textures_no_folder",
        "ERROR: Small textures: no folder {_}: nothing dumped",
    ),
    t(
        "psport.error.small_textures_not_written",
        "ERROR: Small textures: {_} not written",
    ),
    t(
        "psport.error.quirk_workaround_title",
        "ERROR: Quirks: PS_QUIRKS=+{_} ignored: a WORKAROUND applies only to the titles that declare it",
    ),
    t(
        "psport.error.quirk_unknown",
        "ERROR: Quirks: PS_QUIRKS names an unknown quirk '{_}' (psport --list-quirks lists them)",
    ),
    t(
        "psport.error.quirk_syntax",
        "ERROR: Quirks: PS_QUIRKS entry '{_}' ignored: expected none, -<id> or +<id>",
    ),
    t(
        "psport.error.quirk_registry",
        "ERROR: Quirks: {_} names quirk {number}, which is not in the registry",
    ),
    t(
        "psport.error.patch_resolution_unexpected",
        "ERROR: Patches: unexpected resolution patch at {_}",
    ),
    t(
        "psport.error.patch_resolution_viewport",
        "ERROR: Patches: resolution patch is missing scene/UI viewport instructions",
    ),
    t(
        "psport.error.patch_camera_conflict",
        "ERROR: Patches: Restore Debug Camera conflicts with Enemy Control; enable only one",
    ),
    t(
        "psport.error.patch_render_res",
        "ERROR: Patches: PS_RENDER_RES={_}: expected WIDTHxHEIGHT",
    ),
    t(
        "psport.error.patch_intel_tonemap",
        "ERROR: Patches: PS_INTEL_TONEMAP_FIX=1 ignored: quirk bloodborne_intel_tonemap is off",
    ),
    t(
        "psport.error.patch_fps",
        "ERROR: Patches: PS_FPS={_}: expected 30, 60, 90 or uncap",
    ),
    t(
        "psport.error.patch_not_object",
        "ERROR: Patches: {_} is not a JSON object",
    ),
    t("psport.error.patch_other", "ERROR: Patches: {_}"),
    t(
        "psport.error.imgui_link",
        "ERROR: ImGui_ImplOpenGL3_CreateDeviceObjects: failed to link {_}! With GLSL {_}",
    ),
    t(
        "psport.error.imgui_compile",
        "ERROR: ImGui_ImplOpenGL3_CreateDeviceObjects: failed to compile {_}! With GLSL: {_}",
    ),
    t(
        "psport.error.frame_rate_pacer",
        "ERROR: Frame rate: the game's frame pacer is not the expected code: {number} FPS (PS_FRAME_HOOKS=0: the patches)",
    ),
    t(
        "psport.error.frame_rate_memory",
        "ERROR: Frame rate: no memory next to the game's code: {number} FPS (PS_FRAME_HOOKS=0: the patches)",
    ),
    t(
        "psport.error.exec_no_file",
        "ERROR: --exec names no file of the package",
    ),
    t("psport.error.gpu_init", "ERROR: GPU initialization failed"),
    t(
        "psport.error.content_sku",
        "ERROR: PS_CONTENT_SKU must be full or trial",
    ),
    t("psport.error.install_dir_long", "ERROR: PS_INSTALL_DIR too long"),
    t("psport.error.allocation", "ERROR: allocation failed"),
    t("psport.error.import_trap_index", "ERROR: bad import trap index"),
    t("psport.error.patch_entry", "ERROR: bad patch entry"),
    t("psport.error.relocation", "ERROR: bad relocation"),
    t("psport.error.segment", "ERROR: bad segment"),
    t("psport.error.boot_limits", "ERROR: boot file limits exceeded"),
    t("psport.error.guest_stack", "ERROR: cannot allocate guest main stack"),
    t("psport.error.eboot_export", "ERROR: eboot export outside the eboot"),
    t("psport.error.entry_not_executable", "ERROR: entry is not executable"),
    t("psport.error.entry_returned", "ERROR: entry unexpectedly returned"),
    t("psport.error.install_path_long", "ERROR: install path too long"),
    t("psport.error.binding_count", "ERROR: invalid binding count"),
    t("psport.error.tls_metadata", "ERROR: invalid eboot TLS metadata"),
    t("psport.error.linked_metadata", "ERROR: invalid linked module metadata"),
    t("psport.error.module_count", "ERROR: invalid module count"),
    t("psport.error.native_binding", "ERROR: invalid native binding"),
    t(
        "psport.error.sfo_parameter",
        "ERROR: invalid user-defined parameter in param.sfo",
    ),
    t("psport.error.mmap", "ERROR: mmap failed"),
    t("psport.error.module_export", "ERROR: module export outside its module"),
    t(
        "psport.error.module_init_not_executable",
        "ERROR: module init is not executable",
    ),
    t("psport.error.module_initializer", "ERROR: module initializer failed"),
    t(
        "psport.error.native_export_mismatch",
        "ERROR: native export kind/range mismatch",
    ),
    t(
        "psport.error.native_function_not_executable",
        "ERROR: native function is not executable",
    ),
    t("psport.error.patch_relocation", "ERROR: patch overlaps a relocation"),
    t(
        "psport.error.patch_pointer",
        "ERROR: patch writes a pointer outside the image",
    ),
    t(
        "psport.error.vulkan_loader",
        "ERROR: the Vulkan loader library could not be loaded",
    ),
    t(
        "psport.error.runtime_capabilities",
        "ERROR: unknown runtime capabilities",
    ),
    t("psport.error.module_metadata", "ERROR: unmapped module metadata"),
    t("psport.error.native_export_unmapped", "ERROR: unmapped native export"),
    t("psport.error.procparam", "ERROR: unmapped procparam"),
    t("psport.error.import_name", "ERROR: unterminated import name"),
    t(
        "psport.error.mprotect",
        "ERROR: mprotect failed at {_} (+{offset}, flags {number}): {_}",
    ),
    t(
        "psport.host_memory.windows_error",
        "Host memory: {_} failed at {_}: Windows error {number}",
    ),
    t(
        "psport.host_memory.commit_limit",
        "Host memory: cannot commit {number} MiB more guest memory (Windows error {number}): {_}",
    ),
    t("psport.gpu.stubbed", report_gpu_line!("{call}: (STUBBED) called{_}")),
    t(
        "psport.gpu.failed_rc",
        report_gpu_line!("{call}: FAILED rc={code} code={number} {_}"),
    ),
    t(
        "psport.gpu.failed_rc_only",
        report_gpu_line!("{call}: FAILED rc={code}{_}"),
    ),
    t("psport.gpu.assertion", report_gpu_line!("{call}: Assertion Failed!")),
    t(
        "psport.gpu.layer_missing",
        report_gpu_line!("{call}: Requested layer VK_LAYER_KHRONOS_validation is not available"),
    ),
    t(
        "psport.gpu.validation",
        report_gpu_line!("{call}: {vuid}: {call}(): {_}"),
    ),
    t("psport.gpu.validation_message", report_gpu_line!("{call}: {vuid}: {_}")),
    t("psport.gpu.const", report_gpu_line!("{call}: {const}{_}")),
    t("psport.gpu.other", report_gpu_line!("{call}: {_}")),
];
