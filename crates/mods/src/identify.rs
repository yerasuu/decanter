//! Giving a mod added by hand the identity a platform confirms for it afterwards.

use crate::manifest::{ModManifest, ModOrigin, display_name};
use crate::path::ModId;
use crate::{Error, GameMods, Result};

impl GameMods {
    /// Records that mod `id`, added from a file or folder, is `origin`, and names it `name`.
    ///
    /// # Errors
    ///
    /// [`Error::UnknownMod`] for a mod the game does not have, [`Error::InvalidOrigin`] for one that
    /// already has an origin (it was downloaded, so there is nothing to confirm),
    /// [`Error::InvalidName`] for a name that is empty, too long or has control characters, else when
    /// its `mod.toml` can't be read or written.
    #[inline]
    pub fn identify(&self, id: &ModId, origin: ModOrigin, name: &str) -> Result<()> {
        let name = display_name(name).ok_or_else(|| Error::InvalidName(name.to_owned()))?;
        let dir = self.mod_dir(id);
        let mut manifest = ModManifest::read(&dir, id)?.ok_or_else(|| Error::UnknownMod(id.to_string()))?;
        if manifest.origin.is_some() {
            return Err(Error::InvalidOrigin(id.to_string()));
        }
        let platform = origin.platform.clone();
        manifest.name = name;
        manifest.origin = Some(origin);
        manifest.write(&dir)?;
        tracing::info!(mod_id = %id, platform, name = manifest.name, "mod identified");
        Ok(())
    }
}
