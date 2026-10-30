//! The passphrase file, where secrets go when there is no OS keyring to keep them, or the user
//! will not unlock it (decision #1307, design 3.3).
//!
//! `<config>/decanter/secrets.bin`, never the library, which travels with backups and syncs. The
//! key is Argon2id over a passphrase asked once per run and kept only as the derived cipher key,
//! zeroed when dropped; every write seals the whole set of entries again with
//! XChaCha20-Poly1305 under a fresh random nonce. Layout, all integers little-endian:
//!
//! ```text
//! magic "DCNTSEC\0" | version 1 | kdf 1 (Argon2id v0x13) | m_cost KiB u32 | t_cost u32 | p_cost u32
//! | salt 16 | nonce 24 | ciphertext | tag 16
//! ```
//!
//! The header up to the nonce is the AEAD's associated data, so a parameter or salt changed on
//! disk fails like a wrong passphrase instead of being used. The plaintext is a run of records,
//! `name len u32 | name | value len u32 | value`, built with its exact size so that no copy of a
//! value is left behind by a reallocation.
//!
//! On Unix the file is created `0600` in a `0700` folder. On Windows it gets no ACL of its own:
//! setting one takes Win32 calls the workspace's `unsafe_code = forbid` rules out, so it relies
//! on `%APPDATA%`'s, which the user profile already limits to the user, SYSTEM and
//! Administrators.

use core::iter;
use core::ops::RangeInclusive;
use core::str::from_utf8;
use std::fs::{self, OpenOptions};
use std::io::{self, Write as _};
#[cfg(unix)]
use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use argon2::{Algorithm, Argon2, Params, Version};
use chacha20poly1305::{AeadInOut as _, KeyInit as _, Tag, XChaCha20Poly1305, XNonce};
use zeroize::Zeroizing;

use crate::platform;

use super::store::{Backend, BackendKind, Failure, Status, Store};
use super::{Secret, SecretKey};

const MAGIC: [u8; 8] = *b"DCNTSEC\0";
const FORMAT: u8 = 1;
const KDF_ARGON2ID: u8 = 1;
const SALT_LEN: usize = 16;
const NONCE_LEN: usize = 24;
const TAG_LEN: usize = 16;
const AAD_LEN: usize = MAGIC.len() + 2 + 3 * 4 + SALT_LEN;
const HEADER_LEN: usize = AAD_LEN + NONCE_LEN;
const KEY_LEN: usize = 32;
/// A record's two `u32` lengths.
const RECORD_PREFIX_LEN: usize = 2 * size_of::<u32>();

/// Argon2id costs for new files, memory in kibibytes: about half a second on a Steam Deck
/// (0.52 s measured), well above the OWASP floor (19456 kibibytes, two passes), and a one-off
/// quarter gigabyte that such a machine spares while unlocking.
const M_COST_KIB: u32 = 256 * 1024;
const T_COST: u32 = 6;
const P_COST: u32 = 1;

/// What a header's costs are accepted within: below the OWASP floor the file is not one decanter
/// wrote, and above these a damaged header could have unlocking take gigabytes or minutes.
const M_COST_RANGE: RangeInclusive<u32> = 19 * 1024..=1024 * 1024;
const T_COST_RANGE: RangeInclusive<u32> = 2..=32;
const P_COST_RANGE: RangeInclusive<u32> = 1..=8;

/// What the passphrase prompt has to ask for, when the keyring cannot be used.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum FileOffer {
    /// A secrets file is there: its passphrase.
    Unlock,
    /// None yet: a new passphrase, to keep secrets from now on.
    Create,
}

/// Why the secrets file could not be opened or created. None of these carries the passphrase or
/// anything the file holds.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum FileError {
    #[error("no config directory to keep the secrets file in")]
    NoConfigDir,
    #[error("no secrets file to unlock")]
    NotFound,
    #[error("a secrets file already exists")]
    Exists,
    #[error("the passphrase is empty")]
    EmptyPassphrase,
    /// Also what a file altered on disk gives: the cipher cannot tell the two apart. The file is
    /// left as it is.
    #[error("wrong passphrase, or the secrets file was altered")]
    WrongPassphrase,
    #[error("not a secrets file decanter can read: {0}")]
    Corrupt(&'static str),
    #[error("secrets file: {0}")]
    Io(#[from] io::Error),
    #[error("no randomness for the secrets file: {0}")]
    Random(String),
    #[error("secrets over the file's size limit")]
    TooLarge,
    #[error("passphrase key derivation failed: {0}")]
    Kdf(String),
}

/// `<config>/decanter/secrets.bin`.
#[inline]
#[must_use]
pub fn file_path() -> Option<PathBuf> {
    platform::config_dir().map(|dir| dir.join("secrets.bin"))
}

impl Store {
    /// Whether the passphrase file should be offered: the keyring is missing, or locked after its
    /// own unlock prompt was dismissed. Probes the store when nothing has asked it yet, which on
    /// a locked keyring is what shows that prompt first. `None` with a working keyring, and once
    /// the file is in use.
    #[inline]
    #[must_use]
    pub fn file_offer(&self) -> Option<FileOffer> {
        if self.status() == Status::Unknown {
            // Only the store's status is wanted; a failure is already logged and reflected there.
            drop(self.get(&SecretKey::STEAMGRIDDB));
        }
        match self.status() {
            Status::Missing { .. } | Status::Locked { .. } => {
                let path = file_path()?;
                Some(if path.exists() {
                    FileOffer::Unlock
                } else {
                    FileOffer::Create
                })
            }
            Status::Unknown | Status::Ready(_) => None,
        }
    }

    /// Opens the existing secrets file with `passphrase` and keeps it as the store for this run.
    ///
    /// # Errors
    ///
    /// No file, a wrong passphrase, or a file that does not read; the file is never written here,
    /// and the store stays as it was.
    #[inline]
    pub fn unlock_file(&self, passphrase: &Secret) -> Result<(), FileError> {
        let path = file_path().ok_or(FileError::NoConfigDir)?;
        if passphrase.is_empty() {
            return Err(FileError::EmptyPassphrase);
        }
        let bytes = match fs::read(&path) {
            Ok(bytes) => bytes,
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Err(FileError::NotFound),
            Err(err) => return Err(err.into()),
        };
        let (costs, _) = Costs::parse(&bytes).map_err(FileError::Corrupt)?;
        let backend = PassphraseFile {
            cipher: costs.derive(passphrase)?,
            path,
            costs,
            writing: Mutex::new(()),
        };
        backend.open(&bytes).map_err(|err| match err {
            Unsealed::WrongKey => FileError::WrongPassphrase,
            Unsealed::Corrupt(reason) => FileError::Corrupt(reason),
        })?;
        #[cfg(unix)]
        tighten(&backend.path, 0o600)?;
        tracing::info!(path = %backend.path.display(), "secrets file unlocked, secrets kept there for this run");
        self.install(Arc::new(backend));
        Ok(())
    }

    /// Creates an empty secrets file under `passphrase` and keeps it as the store for this run.
    /// Nothing is copied into it from the keyring: what is saved from here on goes there.
    ///
    /// # Errors
    ///
    /// A file already exists (it is left alone), or it could not be written.
    #[inline]
    pub fn create_file(&self, passphrase: &Secret) -> Result<(), FileError> {
        let path = file_path().ok_or(FileError::NoConfigDir)?;
        if passphrase.is_empty() {
            return Err(FileError::EmptyPassphrase);
        }
        if fs::symlink_metadata(&path).is_ok() {
            return Err(FileError::Exists);
        }
        let mut salt = [0; SALT_LEN];
        getrandom::fill(&mut salt).map_err(|err| FileError::Random(err.to_string()))?;
        let costs = Costs {
            m_cost: M_COST_KIB,
            t_cost: T_COST,
            p_cost: P_COST,
            salt,
        };
        let backend = PassphraseFile {
            cipher: costs.derive(passphrase)?,
            path,
            costs,
            writing: Mutex::new(()),
        };
        let sealed = backend.seal(&[])?;
        write_atomic(&backend.path, &sealed)?;
        tracing::info!(path = %backend.path.display(), "secrets file created, secrets kept there for this run");
        self.install(Arc::new(backend));
        Ok(())
    }
}

/// The header's key-derivation part: fixed for the file's life, since the session's key is
/// derived from it once.
struct Costs {
    m_cost: u32,
    t_cost: u32,
    p_cost: u32,
    salt: [u8; SALT_LEN],
}

impl Costs {
    /// The costs and the nonce of a whole file, checked against what decanter writes.
    fn parse(bytes: &[u8]) -> Result<(Self, XNonce), &'static str> {
        let mut reader = Reader(bytes);
        if reader.take(MAGIC.len()) != Some(&MAGIC[..]) {
            return Err("not a decanter secrets file");
        }
        if reader.take(1) != Some(&[FORMAT][..]) {
            return Err("unknown format version");
        }
        if reader.take(1) != Some(&[KDF_ARGON2ID][..]) {
            return Err("unknown key derivation");
        }
        let (Some(m_cost), Some(t_cost), Some(p_cost)) = (reader.u32(), reader.u32(), reader.u32()) else {
            return Err("truncated header");
        };
        if !M_COST_RANGE.contains(&m_cost) || !T_COST_RANGE.contains(&t_cost) || !P_COST_RANGE.contains(&p_cost) {
            return Err("key derivation costs out of range");
        }
        let read_salt = reader
            .take(SALT_LEN)
            .and_then(|raw| <[u8; SALT_LEN]>::try_from(raw).ok());
        let read_nonce = reader.take(NONCE_LEN).and_then(|raw| XNonce::try_from(raw).ok());
        let (Some(salt), Some(nonce)) = (read_salt, read_nonce) else {
            return Err("truncated header");
        };
        if reader.0.len() < TAG_LEN {
            return Err("truncated");
        }
        Ok((
            Self {
                m_cost,
                t_cost,
                p_cost,
                salt,
            },
            nonce,
        ))
    }

    fn write_aad(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&MAGIC);
        out.extend_from_slice(&[FORMAT, KDF_ARGON2ID]);
        out.extend_from_slice(&self.m_cost.to_le_bytes());
        out.extend_from_slice(&self.t_cost.to_le_bytes());
        out.extend_from_slice(&self.p_cost.to_le_bytes());
        out.extend_from_slice(&self.salt);
    }

    fn derive(&self, passphrase: &Secret) -> Result<XChaCha20Poly1305, FileError> {
        let params = Params::new(self.m_cost, self.t_cost, self.p_cost, Some(KEY_LEN))
            .map_err(|err| FileError::Kdf(err.to_string()))?;
        let mut key = Zeroizing::new([0; KEY_LEN]);
        Argon2::new(Algorithm::Argon2id, Version::V0x13, params)
            .hash_password_into(passphrase.expose().as_bytes(), &self.salt, key.as_mut_slice())
            .map_err(|err| FileError::Kdf(err.to_string()))?;
        XChaCha20Poly1305::new_from_slice(key.as_slice()).map_err(|err| FileError::Kdf(err.to_string()))
    }
}

enum Unsealed {
    WrongKey,
    Corrupt(&'static str),
}

struct PassphraseFile {
    path: PathBuf,
    costs: Costs,
    /// Zeroes its key when dropped (`chacha20poly1305`'s `zeroize` feature).
    cipher: XChaCha20Poly1305,
    /// Writes read, change and replace the whole file, so two at once would lose one's entry.
    writing: Mutex<()>,
}

impl PassphraseFile {
    fn seal(&self, plain: &[u8]) -> Result<Zeroizing<Vec<u8>>, FileError> {
        let mut nonce = XNonce::default();
        getrandom::fill(nonce.as_mut_slice()).map_err(|err| FileError::Random(err.to_string()))?;
        let capacity = HEADER_LEN
            .checked_add(plain.len())
            .and_then(|len| len.checked_add(TAG_LEN))
            .ok_or(FileError::TooLarge)?;
        // Zeroizing, since it holds the plaintext until it is encrypted in place.
        let mut out = Zeroizing::new(Vec::with_capacity(capacity));
        self.costs.write_aad(&mut out);
        out.extend_from_slice(&nonce);
        out.extend_from_slice(plain);
        let (header, body) = out.split_at_mut_checked(HEADER_LEN).ok_or(FileError::TooLarge)?;
        let aad = header.get(..AAD_LEN).ok_or(FileError::TooLarge)?;
        let tag = self
            .cipher
            .encrypt_inout_detached(&nonce, aad, body.into())
            .ok()
            .ok_or(FileError::TooLarge)?;
        out.extend_from_slice(&tag);
        Ok(out)
    }

    fn open(&self, bytes: &[u8]) -> Result<Zeroizing<Vec<u8>>, Unsealed> {
        // A salt or costs other than this session's are in the associated data, so such a file
        // fails as a wrong key.
        let (_, nonce) = Costs::parse(bytes).map_err(Unsealed::Corrupt)?;
        let aad = bytes.get(..AAD_LEN).ok_or(Unsealed::Corrupt("truncated header"))?;
        let sealed = bytes.get(HEADER_LEN..).ok_or(Unsealed::Corrupt("truncated header"))?;
        let (body, tag_bytes) = sealed
            .split_at_checked(sealed.len().saturating_sub(TAG_LEN))
            .ok_or(Unsealed::Corrupt("truncated"))?;
        let tag = Tag::try_from(tag_bytes).ok().ok_or(Unsealed::Corrupt("truncated"))?;
        let mut plain = Zeroizing::new(body.to_vec());
        self.cipher
            .decrypt_inout_detached(&nonce, aad, plain.as_mut_slice().into(), &tag)
            .ok()
            .ok_or(Unsealed::WrongKey)?;
        Ok(plain)
    }

    /// The decrypted entries; a file deleted since it was unlocked holds none, and the next
    /// write brings it back under this session's passphrase.
    fn load(&self) -> Result<Zeroizing<Vec<u8>>, Failure> {
        let bytes = match fs::read(&self.path) {
            Ok(bytes) => bytes,
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(Zeroizing::new(Vec::new())),
            Err(err) => return Err(Failure::Refused(format!("secrets file unreadable: {err}"))),
        };
        self.open(&bytes).map_err(|err| match err {
            Unsealed::WrongKey => {
                Failure::Corrupt("secrets file changed on disk, no longer opens with this passphrase")
            }
            Unsealed::Corrupt(reason) => Failure::Corrupt(reason),
        })
    }

    /// Writes the entries of `plain` with `entry` set to `value`, or removed when `None`. A file
    /// that did not open is never reached: `load` failing stops the write first.
    fn rewrite(&self, entry: &str, value: Option<&str>) -> Result<(), Failure> {
        let _writing = self.writing.lock().unwrap_or_else(PoisonError::into_inner);
        let plain = self.load()?;
        if value.is_none() && find(&plain, entry)?.is_none() {
            return Ok(());
        }
        let mut size = value.map_or(Some(0), |new| record_len(entry, new.as_bytes()));
        for record in records(&plain) {
            let (name, kept) = record?;
            if name != entry {
                size = size.and_then(|total| total.checked_add(record_len(name, kept)?));
            }
        }
        let capacity = size.ok_or_else(|| Failure::TooLarge("secrets file over the address space".to_owned()))?;
        let mut next = Zeroizing::new(Vec::with_capacity(capacity));
        for record in records(&plain) {
            let (name, kept) = record?;
            if name != entry {
                push_record(&mut next, name, kept)?;
            }
        }
        if let Some(new) = value {
            push_record(&mut next, entry, new.as_bytes())?;
        }
        let sealed = self.seal(&next).map_err(|err| Failure::Refused(err.to_string()))?;
        write_atomic(&self.path, &sealed).map_err(|err| Failure::Refused(format!("secrets file not written: {err}")))
    }
}

impl Backend for PassphraseFile {
    fn kind(&self) -> BackendKind {
        BackendKind::File
    }

    fn get(&self, entry: &str) -> Result<Option<Secret>, Failure> {
        let plain = self.load()?;
        find(&plain, entry)?
            .map(|value| {
                from_utf8(value)
                    .ok()
                    .map(|text| Secret::from(text.to_owned()))
                    .ok_or(Failure::Corrupt("not UTF-8"))
            })
            .transpose()
    }

    fn put(&self, entry: &str, secret: &Secret) -> Result<(), Failure> {
        self.rewrite(entry, Some(secret.expose()))
    }

    fn delete(&self, entry: &str) -> Result<(), Failure> {
        self.rewrite(entry, None)
    }
}

struct Reader<'bytes>(&'bytes [u8]);

impl<'bytes> Reader<'bytes> {
    fn take(&mut self, len: usize) -> Option<&'bytes [u8]> {
        let (taken, rest) = self.0.split_at_checked(len)?;
        self.0 = rest;
        Some(taken)
    }

    fn u32(&mut self) -> Option<u32> {
        self.take(4)
            .and_then(|bytes| <[u8; 4]>::try_from(bytes).ok())
            .map(u32::from_le_bytes)
    }

    fn sized(&mut self) -> Option<&'bytes [u8]> {
        let len = usize::try_from(self.u32()?).ok()?;
        self.take(len)
    }
}

/// The records of a decrypted file, name and value.
fn records(plain: &[u8]) -> impl Iterator<Item = Result<(&str, &[u8]), Failure>> {
    let mut rest = plain;
    iter::from_fn(move || {
        if rest.is_empty() {
            return None;
        }
        let mut reader = Reader(rest);
        let record = reader.sized().and_then(|name| from_utf8(name).ok()).zip(reader.sized());
        // A record that does not parse ends the run, so its error is yielded once.
        rest = if record.is_some() { reader.0 } else { &[] };
        Some(record.ok_or(Failure::Corrupt("entries do not parse")))
    })
}

fn find<'plain>(plain: &'plain [u8], entry: &str) -> Result<Option<&'plain [u8]>, Failure> {
    for record in records(plain) {
        let (name, value) = record?;
        if name == entry {
            return Ok(Some(value));
        }
    }
    Ok(None)
}

fn record_len(name: &str, value: &[u8]) -> Option<usize> {
    RECORD_PREFIX_LEN.checked_add(name.len())?.checked_add(value.len())
}

fn push_record(out: &mut Vec<u8>, name: &str, value: &[u8]) -> Result<(), Failure> {
    for part in [name.as_bytes(), value] {
        let len = u32::try_from(part.len())
            .ok()
            .ok_or_else(|| Failure::TooLarge("entry over 4 GiB".to_owned()))?;
        out.extend_from_slice(&len.to_le_bytes());
        out.extend_from_slice(part);
    }
    Ok(())
}

/// Written aside with owner-only permissions from its creation, synced and renamed over the old
/// file, so a crash leaves either the old file or the new one, never half of one.
fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let dir = path
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "secrets file has no folder"))?;
    private_dir(dir)?;
    let partial = path.with_extension("bin.part");
    // Left by a write that crashed; `create_new` below refuses to reuse it, and a link put there
    // must not be followed.
    match fs::remove_file(&partial) {
        Ok(()) => {}
        Err(err) if err.kind() == io::ErrorKind::NotFound => {}
        Err(err) => return Err(err),
    }
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.mode(0o600);
    let written = options.open(&partial).and_then(|mut file| {
        file.write_all(bytes)?;
        file.sync_all()
    });
    if let Err(err) = written.and_then(|()| fs::rename(&partial, path)) {
        drop(fs::remove_file(&partial));
        return Err(err);
    }
    // Windows opens no folder as a file to sync; NTFS journals the rename itself.
    #[cfg(unix)]
    fs::File::open(dir)?.sync_all()?;
    Ok(())
}

#[cfg(unix)]
fn private_dir(dir: &Path) -> io::Result<()> {
    fs::DirBuilder::new().recursive(true).mode(0o700).create(dir)?;
    tighten(dir, 0o700)
}

#[cfg(not(unix))]
fn private_dir(dir: &Path) -> io::Result<()> {
    fs::create_dir_all(dir)
}

/// Narrows `path` to `mode` when it grants anything to group or others: a folder made by
/// something else, or a file copied in by hand.
#[cfg(unix)]
fn tighten(path: &Path, mode: u32) -> io::Result<()> {
    let current = fs::metadata(path)?.permissions().mode();
    if current & 0o077 != 0 {
        fs::set_permissions(path, fs::Permissions::from_mode(mode))?;
        tracing::warn!(path = %path.display(), was = format_args!("{:04o}", current & 0o7777), "loose permissions narrowed");
    }
    Ok(())
}
