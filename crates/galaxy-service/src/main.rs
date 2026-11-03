// SPDX-License-Identifier: GPL-3.0-or-later
//! The `GalaxyCommunication` Windows service, as a stand-in.
//!
//! Some Galaxy SDK builds look for this service, or ask the service control
//! manager to start it, before they go on. Under Wine nothing real stands
//! behind it, so decanter registers this one in the game's prefix: it
//! registers with the SCM, reports `RUNNING`, honours `STOP` and does
//! nothing else, with no network and no listener. The local Galaxy service
//! that answers the SDK is a separate piece of decanter.
//!
//! Dependency-free on purpose: three `advapi32` calls and an event are the
//! whole surface, so a service crate would only add code to audit.

use core::ffi::c_void;
use core::ptr;
use core::sync::atomic::{AtomicPtr, AtomicU32, Ordering};
use std::process::ExitCode;

const SERVICE_NAME: [u16; 19] = utf16("GalaxyCommunication");

const SERVICE_WIN32_OWN_PROCESS: u32 = 0x10;
const SERVICE_STOPPED: u32 = 1;
const SERVICE_START_PENDING: u32 = 2;
const SERVICE_STOP_PENDING: u32 = 3;
const SERVICE_RUNNING: u32 = 4;
const SERVICE_ACCEPT_STOP: u32 = 1;
const SERVICE_CONTROL_STOP: u32 = 1;
const SERVICE_CONTROL_SHUTDOWN: u32 = 5;
const INFINITE: u32 = u32::MAX;
const PENDING_WAIT_HINT_MS: u32 = 3000;

type Handle = *mut c_void;

#[repr(C)]
struct ServiceTableEntry {
    name: *const u16,
    main: Option<unsafe extern "system" fn(u32, *mut *mut u16)>,
}

#[repr(C)]
struct ServiceStatus {
    service_type: u32,
    current_state: u32,
    controls_accepted: u32,
    win32_exit_code: u32,
    service_specific_exit_code: u32,
    check_point: u32,
    wait_hint: u32,
}

#[link(name = "advapi32")]
unsafe extern "system" {
    fn StartServiceCtrlDispatcherW(table: *const ServiceTableEntry) -> i32;
    fn RegisterServiceCtrlHandlerW(name: *const u16, handler: Option<unsafe extern "system" fn(u32)>) -> Handle;
    fn SetServiceStatus(status_handle: Handle, status: *const ServiceStatus) -> i32;
}

#[link(name = "kernel32")]
unsafe extern "system" {
    fn CreateEventW(attributes: *const c_void, is_manual_reset: i32, is_signaled: i32, name: *const u16) -> Handle;
    fn SetEvent(event: Handle) -> i32;
    fn WaitForSingleObject(handle: Handle, milliseconds: u32) -> u32;
}

static STATUS_HANDLE: AtomicPtr<c_void> = AtomicPtr::new(ptr::null_mut());
static STOP_EVENT: AtomicPtr<c_void> = AtomicPtr::new(ptr::null_mut());
static STATE: AtomicU32 = AtomicU32::new(SERVICE_STOPPED);

const fn utf16<const N: usize>(text: &str) -> [u16; N] {
    let bytes = text.as_bytes();
    let mut out = [0u16; N];
    let mut i = 0;
    while i < bytes.len() && i + 1 < N {
        out[i] = bytes[i] as u16;
        i += 1;
    }
    out
}

fn report(state: u32, check_point: u32) {
    STATE.store(state, Ordering::SeqCst);
    let status = ServiceStatus {
        service_type: SERVICE_WIN32_OWN_PROCESS,
        current_state: state,
        controls_accepted: if state == SERVICE_RUNNING {
            SERVICE_ACCEPT_STOP
        } else {
            0
        },
        win32_exit_code: 0,
        service_specific_exit_code: 0,
        check_point,
        wait_hint: if check_point == 0 { 0 } else { PENDING_WAIT_HINT_MS },
    };
    // SAFETY: the handle came from `RegisterServiceCtrlHandlerW` and `status` is a valid, live struct.
    unsafe { SetServiceStatus(STATUS_HANDLE.load(Ordering::SeqCst), &raw const status) };
}

unsafe extern "system" fn control_handler(control: u32) {
    if control == SERVICE_CONTROL_STOP || control == SERVICE_CONTROL_SHUTDOWN {
        report(SERVICE_STOP_PENDING, 1);
        // SAFETY: the event is created before this handler is registered and never closed.
        unsafe { SetEvent(STOP_EVENT.load(Ordering::SeqCst)) };
    } else {
        // Interrogate and everything else: repeat the current state.
        report(STATE.load(Ordering::SeqCst), 0);
    }
}

unsafe extern "system" fn service_main(_argc: u32, _argv: *mut *mut u16) {
    // SAFETY: a manual-reset, initially non-signalled, unnamed event; the arguments are plain values.
    let event = unsafe { CreateEventW(ptr::null(), 1, 0, ptr::null()) };
    if event.is_null() {
        return;
    }
    STOP_EVENT.store(event, Ordering::SeqCst);
    // SAFETY: `SERVICE_NAME` is NUL-terminated and `control_handler` has the signature the SCM calls.
    let handle = unsafe { RegisterServiceCtrlHandlerW(SERVICE_NAME.as_ptr(), Some(control_handler)) };
    if handle.is_null() {
        return;
    }
    STATUS_HANDLE.store(handle, Ordering::SeqCst);
    report(SERVICE_START_PENDING, 1);
    report(SERVICE_RUNNING, 0);
    // SAFETY: `event` is the live handle created above.
    unsafe { WaitForSingleObject(event, INFINITE) };
    report(SERVICE_STOPPED, 0);
}

fn main() -> ExitCode {
    let table = [
        ServiceTableEntry {
            name: SERVICE_NAME.as_ptr(),
            main: Some(service_main),
        },
        ServiceTableEntry {
            name: ptr::null(),
            main: None,
        },
    ];
    // SAFETY: `table` is a NULL-terminated array that outlives the call, which blocks until the service ends.
    // It fails with ERROR_FAILED_SERVICE_CONTROLLER_CONNECT when run outside the SCM, e.g. from a console.
    if unsafe { StartServiceCtrlDispatcherW(table.as_ptr()) } == 0 {
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}
