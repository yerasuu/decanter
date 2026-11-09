//! DLL overrides a mod needs under Wine (design, section 4): a proxy named after a DLL Wine ships a
//! builtin for is not loaded from the game's folder unless `WINEDLLOVERRIDES` says native first,
//! per the [Wine User's Guide](https://gitlab.winehq.org/wine/wine/-/wikis/Wine-User's-Guide#winedlloverrides-dll-overrides).

use core::fmt;
use core::str::FromStr;
use std::collections::HashSet;

use crate::manifest::ModManifest;
use crate::path::{ModId, ModPath};
use crate::{Error, Result};

/// The system DLLs mods most often ship a proxy of (ASI loaders, shader injectors, input wrappers).
const PROXIES: [&str; 9] = [
    "dinput8",
    "version",
    "winmm",
    "dxgi",
    "d3d9",
    "d3d11",
    "dsound",
    "xinput1_3",
    "winhttp",
];
const MAX_DLL_CHARS: usize = 64;

/// How Wine loads one DLL, as `WINEDLLOVERRIDES` spells it: the whole of Wine's grammar, so it is
/// not expected to grow.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[expect(
    clippy::exhaustive_enums,
    reason = "Wine's whole grammar for one DLL, matched without a wildcard"
)]
pub enum DllMode {
    /// `n,b`: the game folder's copy first, Wine's own if there is none.
    NativeThenBuiltin,
    /// `b,n`: Wine's own first.
    BuiltinThenNative,
    /// `n`: only the game folder's copy.
    Native,
    /// `b`: only Wine's own.
    Builtin,
    /// Empty: not loaded at all.
    Disabled,
}

impl DllMode {
    pub const ALL: [Self; 5] = [
        Self::NativeThenBuiltin,
        Self::BuiltinThenNative,
        Self::Native,
        Self::Builtin,
        Self::Disabled,
    ];

    #[must_use]
    #[inline]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NativeThenBuiltin => "n,b",
            Self::BuiltinThenNative => "b,n",
            Self::Native => "n",
            Self::Builtin => "b",
            Self::Disabled => "",
        }
    }
}

impl FromStr for DllMode {
    type Err = Error;

    #[inline]
    fn from_str(text: &str) -> Result<Self> {
        Self::ALL
            .into_iter()
            .find(|mode| mode.as_str() == text)
            .ok_or_else(|| Error::InvalidOverride(text.to_owned()))
    }
}

impl fmt::Display for DllMode {
    #[inline]
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One DLL a mod wants Wine to load its way.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct DllOverride {
    /// Lowercase, without `.dll`: the key `WINEDLLOVERRIDES` takes.
    pub dll: String,
    pub mode: DllMode,
}

impl DllOverride {
    /// # Errors
    ///
    /// [`Error::InvalidOverride`] for a name that is empty, longer than 64 characters or holds
    /// anything but ASCII letters, digits, `_`, `-` and `.`, since `;`, `=` and `,` would change
    /// what else `WINEDLLOVERRIDES` says.
    #[inline]
    pub fn new(dll: &str, mode: DllMode) -> Result<Self> {
        let lower = dll.to_ascii_lowercase();
        let name = lower.strip_suffix(".dll").unwrap_or(&lower);
        if name.is_empty()
            || name.len() > MAX_DLL_CHARS
            || !name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
        {
            return Err(Error::InvalidOverride(dll.to_owned()));
        }
        Ok(Self {
            dll: name.to_owned(),
            mode,
        })
    }
}

/// `n,b` for every proxy DLL among `files` (paths in the game) that sits in a folder the game
/// starts a program from.
pub(crate) fn suggest<'path>(
    files: impl Iterator<Item = &'path ModPath>,
    program_dirs: &HashSet<String>,
) -> Vec<DllOverride> {
    let mut found = Vec::new();
    for path in files {
        let name = path.file_name().to_ascii_lowercase();
        let Some(stem) = name.strip_suffix(".dll") else {
            continue;
        };
        if !PROXIES.contains(&stem) || found.iter().any(|known: &DllOverride| known.dll == stem) {
            continue;
        }
        let folded = path.folded();
        let dir = folded.rsplit_once('/').map_or("", |(dir, _)| dir);
        if program_dirs.contains(dir) {
            found.push(DllOverride {
                dll: stem.to_owned(),
                mode: DllMode::NativeThenBuiltin,
            });
        }
    }
    found
}

/// The folded folders of the install that hold a Windows program, where a proxy DLL is loaded
/// from.
pub(crate) fn program_dirs(install_files: &[ModPath]) -> HashSet<String> {
    install_files
        .iter()
        .filter(|path| {
            std::path::Path::new(path.file_name())
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("exe"))
        })
        .map(|path| {
            let folded = path.folded();
            folded
                .rsplit_once('/')
                .map_or_else(String::new, |(dir, _)| dir.to_owned())
        })
        .collect()
}

/// The override one DLL ends up with for an account: the last enabled mod asking for it wins.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct EffectiveOverride {
    pub dll: String,
    pub mode: DllMode,
    pub from: ModId,
    /// Earlier enabled mods that asked for another mode, in load order.
    pub over: Vec<(ModId, DllMode)>,
}

/// What `mods`, in load order, ask Wine for, one entry per DLL in the order first asked.
pub(crate) fn effective(mods: &[ModManifest]) -> Vec<EffectiveOverride> {
    let mut found: Vec<EffectiveOverride> = Vec::new();
    for manifest in mods {
        for wanted in &manifest.dll_overrides {
            if let Some(known) = found.iter_mut().find(|known| known.dll == wanted.dll) {
                if known.mode != wanted.mode {
                    let previous = core::mem::replace(&mut known.from, manifest.id.clone());
                    known.over.push((previous, known.mode));
                }
                known.from.clone_from(&manifest.id);
                known.mode = wanted.mode;
            } else {
                found.push(EffectiveOverride {
                    dll: wanted.dll.clone(),
                    mode: wanted.mode,
                    from: manifest.id.clone(),
                    over: Vec::new(),
                });
            }
        }
    }
    found
}
