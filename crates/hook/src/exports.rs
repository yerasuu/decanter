//! Every export any known Galaxy64 version has, forwarded so the game
//! links and runs exactly as before. This is deliberately the union
//! across versions rather than one version's list: a game linked against
//! a newer SDK imports symbols an older DLL never had, and a proxy
//! missing even one of them fails to load, taking the game down before a
//! single line of ours runs. That is not hypothetical -- it is what a
//! 31-export game did to a 28-export proxy. Forwarding a symbol the real
//! DLL lacks is harmless: the lookup fails once, is cached, and the stub
//! returns null or does nothing.

use std::ffi::c_void;
use std::sync::atomic::AtomicUsize;

use crate::ffi::real_proc;
use crate::hook::install_hook;
use crate::log::trace_once;

/// An accessor returning an interface pointer -- the shape of most Galaxy
/// exports.
macro_rules! accessor {
    ($rust_name:ident, $symbol:literal) => {
        #[unsafe(export_name = $symbol)]
        pub extern "C" fn $rust_name() -> *mut c_void {
            static CACHE: AtomicUsize = AtomicUsize::new(0);
            static SEEN: AtomicUsize = AtomicUsize::new(0);
            trace_once(&SEEN, concat!("enter ", $symbol));
            let target = real_proc(&CACHE, concat!($symbol, "\0"));
            if target.is_null() {
                return std::ptr::null_mut();
            }
            // SAFETY: `target` is a non-null export resolved by symbol name
            // from the real Galaxy64, whose accessor exports all share this
            // `() -> *mut c_void` signature.
            let target: extern "C" fn() -> *mut c_void = unsafe { std::mem::transmute(target) };
            target()
        }
    };
}

/// `void f()` -- the process/shutdown pumps.
macro_rules! procedure {
    ($rust_name:ident, $symbol:literal) => {
        #[unsafe(export_name = $symbol)]
        pub extern "C" fn $rust_name() {
            static CACHE: AtomicUsize = AtomicUsize::new(0);
            static SEEN: AtomicUsize = AtomicUsize::new(0);
            trace_once(&SEEN, concat!("enter ", $symbol));
            let target = real_proc(&CACHE, concat!($symbol, "\0"));
            if target.is_null() {
                return;
            }
            // SAFETY: `target` is a non-null export resolved by symbol name
            // from the real Galaxy64, whose `void f()` exports all share
            // this signature.
            let target: extern "C" fn() = unsafe { std::mem::transmute(target) };
            target()
        }
    };
}

/// `void f(const T&)` -- a reference is a pointer at this level, which is
/// all the forwarding needs to know. Covers both `InitOptions` and the
/// newer `ShutdownOptions`.
macro_rules! with_options {
    ($rust_name:ident, $symbol:literal) => {
        #[unsafe(export_name = $symbol)]
        pub extern "C" fn $rust_name(options: *const c_void) {
            static CACHE: AtomicUsize = AtomicUsize::new(0);
            static SEEN: AtomicUsize = AtomicUsize::new(0);
            trace_once(&SEEN, concat!("enter ", $symbol));
            let target = real_proc(&CACHE, concat!($symbol, "\0"));
            if target.is_null() {
                return;
            }
            // SAFETY: `target` is a non-null export resolved by symbol name
            // from the real Galaxy64, whose `void f(const T&)` exports all
            // share this signature (a reference is a pointer at this ABI
            // level).
            let target: extern "C" fn(*const c_void) = unsafe { std::mem::transmute(target) };
            target(options)
        }
    };
}

/// The one export that isn't a pure forward: the game asks for `IStats`
/// here, which is the only moment a real `IStats` pointer exists to patch.
#[unsafe(export_name = "?Stats@api@galaxy@@YAPEAVIStats@12@XZ")]
pub extern "C" fn stats() -> *mut c_void {
    static CACHE: AtomicUsize = AtomicUsize::new(0);
    static SEEN: AtomicUsize = AtomicUsize::new(0);
    trace_once(&SEEN, "enter ?Stats@api@galaxy@@YAPEAVIStats@12@XZ");
    let target = real_proc(&CACHE, "?Stats@api@galaxy@@YAPEAVIStats@12@XZ\0");
    if target.is_null() {
        return std::ptr::null_mut();
    }
    // SAFETY: `target` is the non-null `?Stats@...` export resolved by name
    // from the real Galaxy64, matching this `() -> *mut c_void` signature.
    let target: extern "C" fn() -> *mut c_void = unsafe { std::mem::transmute(target) };
    let stats = target();
    install_hook(stats);
    stats
}

accessor!(apps, "?Apps@api@galaxy@@YAPEAVIApps@12@XZ");
accessor!(chat, "?Chat@api@galaxy@@YAPEAVIChat@12@XZ");
accessor!(
    cloud_storage,
    "?CloudStorage@api@galaxy@@YAPEAVICloudStorage@12@XZ"
);
accessor!(
    custom_networking,
    "?CustomNetworking@api@galaxy@@YAPEAVICustomNetworking@12@XZ"
);
accessor!(friends, "?Friends@api@galaxy@@YAPEAVIFriends@12@XZ");
accessor!(
    game_server_listener_registrar,
    "?GameServerListenerRegistrar@api@galaxy@@YAPEAVIListenerRegistrar@12@XZ"
);
accessor!(
    game_server_logger,
    "?GameServerLogger@api@galaxy@@YAPEAVILogger@12@XZ"
);
accessor!(
    game_server_matchmaking,
    "?GameServerMatchmaking@api@galaxy@@YAPEAVIMatchmaking@12@XZ"
);
accessor!(
    game_server_networking,
    "?GameServerNetworking@api@galaxy@@YAPEAVINetworking@12@XZ"
);
accessor!(
    game_server_telemetry,
    "?GameServerTelemetry@api@galaxy@@YAPEAVITelemetry@12@XZ"
);
accessor!(
    game_server_user,
    "?GameServerUser@api@galaxy@@YAPEAVIUser@12@XZ"
);
accessor!(
    game_server_utils,
    "?GameServerUtils@api@galaxy@@YAPEAVIUtils@12@XZ"
);
accessor!(get_error, "?GetError@api@galaxy@@YAPEBVIError@12@XZ");
accessor!(
    listener_registrar,
    "?ListenerRegistrar@api@galaxy@@YAPEAVIListenerRegistrar@12@XZ"
);
accessor!(logger, "?Logger@api@galaxy@@YAPEAVILogger@12@XZ");
accessor!(
    matchmaking,
    "?Matchmaking@api@galaxy@@YAPEAVIMatchmaking@12@XZ"
);
accessor!(networking, "?Networking@api@galaxy@@YAPEAVINetworking@12@XZ");
accessor!(storage, "?Storage@api@galaxy@@YAPEAVIStorage@12@XZ");
accessor!(telemetry, "?Telemetry@api@galaxy@@YAPEAVITelemetry@12@XZ");
accessor!(user, "?User@api@galaxy@@YAPEAVIUser@12@XZ");
accessor!(utils, "?Utils@api@galaxy@@YAPEAVIUtils@12@XZ");

procedure!(process_data, "?ProcessData@api@galaxy@@YAXXZ");
procedure!(
    process_game_server_data,
    "?ProcessGameServerData@api@galaxy@@YAXXZ"
);
procedure!(shutdown, "?Shutdown@api@galaxy@@YAXXZ");
procedure!(shutdown_game_server, "?ShutdownGameServer@api@galaxy@@YAXXZ");
// The real one is a bare `ret` -- kept only so the export table matches.
procedure!(load, "load");

with_options!(init, "?Init@api@galaxy@@YAXAEBUInitOptions@12@@Z");
with_options!(
    init_game_server,
    "?InitGameServer@api@galaxy@@YAXAEBUInitOptions@12@@Z"
);
with_options!(
    shutdown_ex,
    "?ShutdownEx@api@galaxy@@YAXAEBUShutdownOptions@12@@Z"
);
with_options!(
    shutdown_game_server_ex,
    "?ShutdownGameServerEx@api@galaxy@@YAXAEBUShutdownOptions@12@@Z"
);
