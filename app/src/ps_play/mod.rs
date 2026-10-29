//! Playing a PS game outside GPUI: the launch's inputs and command ([`launch`]), `psport.exe` started
//! directly where there is no toolchain ([`direct`]), and the run's log, scan and records ([`run`]).
//! The window's Play and `decanter ps run` both go through here, so a headless run starts a game and
//! records its failures the way Play does.

#[cfg(windows)]
pub mod direct;
pub mod launch;
pub mod run;
