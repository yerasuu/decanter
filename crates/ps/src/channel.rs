//! Which builds of a port a library runs, chosen in the PS section's Ajustes.
//!
//! Release keeps the checkout on the tag the port's CI last published
//! ([`crate::os::linux::prebuilt::latest_release`]) and runs that tag's bundle
//! ([`crate::os::linux::bundle`]), or fetches its build from the nix cache; Testing
//! follows the default branch and fetches each commit CI built from the same
//! cache, building locally whatever it lacks. Only a port
//! whose CI publishes builds has a choice: every other one is always on
//! Testing.

use decanter_core::Db;

use crate::paths::Root;
use crate::port::Port;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Channel {
    Release,
    Testing,
}

impl Channel {
    /// Blocking database read: off the UI thread. A library whose database
    /// can't be read gets the default, Release.
    pub fn of(root: &Root, port: Port) -> Self {
        if port.binary_cache().is_none() {
            return Self::Testing;
        }
        let testing = Db::open(decanter_core::bootstrap::db_path(root.library())).and_then(|db| db.psport_testing());
        match testing {
            Ok(true) => Self::Testing,
            Ok(false) => Self::Release,
            Err(err) => {
                tracing::warn!(library = %root.library().display(), %err, "could not read the psport channel; using Release");
                Self::Release
            }
        }
    }

    /// Blocking database write: off the UI thread.
    pub fn save(self, root: &Root) -> decanter_core::Result<()> {
        Db::open(decanter_core::bootstrap::db_path(root.library()))
            .and_then(|db| db.set_psport_testing(self == Self::Testing))
    }
}
