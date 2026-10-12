//! A game's keyboard and mouse bindings, where its port keeps them: bbport
//! as `key.` lines in its `bbport.ini`, AnyPS5 in the game's
//! `anyps5-input.ini`. Both are file I/O: off the UI thread.

use std::io;
use std::path::Path;

use crate::bbport::{self, Ini};
use crate::mapping::{Mapping, Target};

/// The file over the port's defaults. One that does not parse is an
/// error, so it is never overwritten with what decanter understood of it.
pub fn read(target: Target, path: &Path) -> Result<Mapping, String> {
    let defaults = target.defaults();
    if target == Target::Bbport {
        return Ok(bbport::read_binds(&Ini::load(path), &defaults));
    }
    match std::fs::read_to_string(path) {
        Ok(text) => Mapping::parse(&text, &defaults).map_err(|err| format!("{}: {err}", path.display())),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(defaults),
        Err(err) => Err(format!("{}: {err}", path.display())),
    }
}

/// `bbport.ini` is read again right before writing, so settings changed
/// elsewhere in it since [`read`] survive.
pub fn save(target: Target, path: &Path, mapping: &Mapping) -> io::Result<()> {
    let defaults = target.defaults();
    if target == Target::Bbport {
        let mut ini = Ini::load(path);
        bbport::write_binds(&mut ini, mapping, &defaults);
        return ini.save(path);
    }
    crate::files::write_atomic(path, mapping.render(&defaults).as_bytes())
}
