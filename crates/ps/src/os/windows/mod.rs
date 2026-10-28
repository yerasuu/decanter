//! Windows: a release's package, or a branch built with decanter's pinned toolchain.

pub mod package;
#[cfg(windows)]
pub(super) mod sys;
pub mod winbuild;
