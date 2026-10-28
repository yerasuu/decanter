//! What getting a port onto each system takes, one module per OS.
//!
//! Each compiles on every system so that one system's `cargo check` also covers the others'
//! orchestration; which one runs is decided at run time ([`decanter_core::platform::support`]).

pub mod linux;
pub mod windows;
