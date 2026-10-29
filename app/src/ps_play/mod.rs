//! Playing a PS game outside GPUI: the launch's inputs and command ([`launch`]), and the run's log,
//! scan and records ([`run`]). The window's Play and `decanter ps run` both go through here, so a
//! headless run starts a game and records its failures the way Play does.

pub mod launch;
pub mod run;
