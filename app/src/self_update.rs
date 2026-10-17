//! Replaces the AppImage decanter runs from with a newer release, once its
//! minisign signature checks out against the key built in from
//! `keys/release.pub` (see build.rs). Releases are signed by hand next to
//! their tag, never by the CI, so a compromised workflow can't ship an
//! update on its own (decision #400). Anywhere else (a nix-shell, `cargo
//! run`, /nix/store) decanter doesn't rewrite itself, and Ajustes only links
//! the release page.

use std::borrow::Cow;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;
use std::time::Duration;

use minisign_verify::{PublicKey, Signature};
use rust_i18n::t;
use tokio::io::AsyncWriteExt as _;

use crate::releases::DECANTER_REPO;

const RELEASE_KEY: &str = include_str!(concat!(env!("OUT_DIR"), "/release.pub"));
/// The asset the CI uploads to every release (scripts/appimage, ci.yml).
const ASSET: &str = "Decanter-x86_64.AppImage";
const MAX_APPIMAGE: u64 = 512 * 1024 * 1024;
const MAX_SIGNATURE: usize = 4096;
/// GitHub answers a release download with a redirect to its asset storage.
const DOWNLOAD_HOSTS: [&str; 3] = [
    "github.com",
    "objects.githubusercontent.com",
    "release-assets.githubusercontent.com",
];
/// How often the download reports progress, so the UI isn't redrawn per chunk.
const PROGRESS_STEP: u64 = 1024 * 1024;

static RESTART: OnceLock<PathBuf> = OnceLock::new();

/// Whether this decanter can install a release over itself.
#[derive(Debug, Clone)]
pub enum Availability {
    /// Running from this AppImage, in a folder decanter may write to.
    AppImage(PathBuf),
    /// An AppImage it could replace, but this build carries no key to verify a release with.
    NoKey,
    /// Not an AppImage decanter can rewrite.
    Elsewhere,
}

fn in_nix_store(path: &Path) -> bool {
    path.starts_with("/nix/store")
}

/// `$APPIMAGE` is set by the AppImage runtime, but anything can set it: it
/// only counts as an AppImage when it names an existing file in a writable
/// folder, outside the nix store.
pub fn availability() -> Availability {
    if !cfg!(target_os = "linux") {
        return Availability::Elsewhere;
    }
    let Some(appimage) = std::env::var_os("APPIMAGE").filter(|value| !value.is_empty()) else {
        return Availability::Elsewhere;
    };
    let appimage = PathBuf::from(appimage);
    let exe_in_store = std::env::current_exe().is_ok_and(|exe| in_nix_store(&exe));
    let appimage = match appimage.canonicalize() {
        Ok(path) if path.is_file() && !in_nix_store(&path) && !exe_in_store => path,
        Ok(path) => {
            tracing::info!(appimage = %path.display(), exe_in_store, "not an AppImage decanter can replace");
            return Availability::Elsewhere;
        }
        Err(err) => {
            tracing::warn!(appimage = %appimage.display(), %err, "APPIMAGE names no file");
            return Availability::Elsewhere;
        }
    };
    let Some(dir) = appimage.parent() else {
        return Availability::Elsewhere;
    };
    if let Err(err) = crate::platform::check_writable(dir) {
        tracing::info!(dir = %dir.display(), %err, "the AppImage's folder isn't writable");
        return Availability::Elsewhere;
    }
    if RELEASE_KEY.is_empty() {
        tracing::info!("this build carries no release key, updates can't be verified");
        return Availability::NoKey;
    }
    Availability::AppImage(appimage)
}

#[derive(Debug, thiserror::Error)]
pub enum InstallError {
    #[error("no connection to GitHub: {0}")]
    Offline(String),
    #[error("release {0} has no AppImage")]
    MissingAppImage(String),
    #[error("release {0} isn't signed yet")]
    Unsigned(String),
    #[error("download failed: {0}")]
    Download(String),
    #[error("the download is larger than {limit} bytes")]
    TooLarge { limit: u64 },
    #[error("the signature file is malformed: {0}")]
    MalformedSignature(String),
    #[error("signed with a key this build doesn't trust")]
    WrongKey,
    #[error("the signature doesn't match the downloaded AppImage")]
    BadSignature,
    #[error("signed as {found:?}, expected {expected:?}")]
    WrongRelease { expected: String, found: String },
    #[error("no permission to write in {0}")]
    NoPermission(String),
    #[error("no space left in {0}")]
    DiskFull(String),
    #[error("cannot write {path}: {error}")]
    Io { path: String, error: String },
    #[error("the release key built in doesn't parse: {0}")]
    BrokenKey(String),
}

impl InstallError {
    pub fn localized(&self) -> Cow<'static, str> {
        match self {
            Self::Offline(_) => t!("settings.updates.errors.offline"),
            Self::MissingAppImage(tag) => t!("settings.updates.install_errors.missing_appimage", tag = tag),
            Self::Unsigned(tag) => t!("settings.updates.install_errors.unsigned", tag = tag),
            Self::Download(detail) => t!("settings.updates.install_errors.download", error = detail),
            Self::TooLarge { limit } => {
                t!(
                    "settings.updates.install_errors.too_large",
                    size = crate::format::bytes(*limit)
                )
            }
            Self::MalformedSignature(_) | Self::BadSignature => t!("settings.updates.install_errors.bad_signature"),
            Self::WrongKey => t!("settings.updates.install_errors.wrong_key"),
            Self::WrongRelease { found, .. } => t!("settings.updates.install_errors.wrong_release", found = found),
            Self::NoPermission(dir) => t!("settings.updates.install_errors.no_permission", dir = dir),
            Self::DiskFull(dir) => t!("settings.updates.install_errors.disk_full", dir = dir),
            Self::Io { path, error } => t!("settings.updates.install_errors.io", path = path, error = error),
            Self::BrokenKey(detail) => t!("settings.updates.install_errors.broken_key", error = detail),
        }
    }
}

fn network_error(err: &reqwest::Error) -> InstallError {
    if err.is_connect() || err.is_timeout() {
        InstallError::Offline(err.to_string())
    } else {
        InstallError::Download(err.to_string())
    }
}

fn write_error(path: &Path, err: &std::io::Error) -> InstallError {
    let dir = || path.parent().unwrap_or(path).display().to_string();
    match err.kind() {
        std::io::ErrorKind::PermissionDenied | std::io::ErrorKind::ReadOnlyFilesystem => {
            InstallError::NoPermission(dir())
        }
        std::io::ErrorKind::StorageFull | std::io::ErrorKind::QuotaExceeded | std::io::ErrorKind::FileTooLarge => {
            InstallError::DiskFull(dir())
        }
        // `ErrorKind` is non-exhaustive; every other failure is reported as it is.
        _ => InstallError::Io {
            path: path.display().to_string(),
            error: err.to_string(),
        },
    }
}

fn signature_error(err: &minisign_verify::Error) -> InstallError {
    match err {
        minisign_verify::Error::UnexpectedKeyId => InstallError::WrongKey,
        minisign_verify::Error::InvalidSignature => InstallError::BadSignature,
        minisign_verify::Error::InvalidEncoding
        | minisign_verify::Error::IoError(_)
        | minisign_verify::Error::UnexpectedAlgorithm
        | minisign_verify::Error::UnsupportedAlgorithm
        | minisign_verify::Error::UnsupportedLegacyMode => InstallError::MalformedSignature(err.to_string()),
    }
}

fn client() -> Result<reqwest::Client, InstallError> {
    reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(20))
        .read_timeout(Duration::from_secs(60))
        .user_agent(concat!("decanter/", env!("CARGO_PKG_VERSION")))
        .redirect(reqwest::redirect::Policy::custom(|attempt| {
            let allowed = attempt.url().scheme() == "https"
                && attempt
                    .url()
                    .host_str()
                    .is_some_and(|host| DOWNLOAD_HOSTS.contains(&host));
            if attempt.previous().len() >= 5 {
                attempt.error("too many redirects")
            } else if allowed {
                attempt.follow()
            } else {
                tracing::warn!(url = %attempt.url(), "release download redirected off GitHub");
                attempt.error("redirected off GitHub's release storage")
            }
        }))
        .build()
        .map_err(|err| InstallError::Download(err.to_string()))
}

/// `None` when the release has no such asset.
async fn asset(client: &reqwest::Client, tag: &str, name: &str) -> Result<Option<reqwest::Response>, InstallError> {
    let url = format!("https://github.com/{DECANTER_REPO}/releases/download/{tag}/{name}");
    let response = client.get(&url).send().await.map_err(|err| network_error(&err))?;
    if response.status() == reqwest::StatusCode::NOT_FOUND {
        tracing::warn!(url, "release asset not found");
        return Ok(None);
    }
    let response = response.error_for_status().map_err(|err| network_error(&err))?;
    Ok(Some(response))
}

async fn signature(client: &reqwest::Client, tag: &str) -> Result<Signature, InstallError> {
    let Some(mut response) = asset(client, tag, &format!("{ASSET}.minisig")).await? else {
        return Err(InstallError::Unsigned(tag.to_owned()));
    };
    let mut text = Vec::with_capacity(1024);
    while let Some(chunk) = response.chunk().await.map_err(|err| network_error(&err))? {
        text.extend_from_slice(&chunk);
        if text.len() > MAX_SIGNATURE {
            return Err(InstallError::MalformedSignature(format!(
                "larger than {MAX_SIGNATURE} bytes"
            )));
        }
    }
    let text = String::from_utf8(text).map_err(|err| InstallError::MalformedSignature(err.to_string()))?;
    Signature::decode(&text).map_err(|err| signature_error(&err))
}

/// Downloads release `tag`'s AppImage next to `appimage`, verifies it and
/// renames it over `appimage`. On any failure `appimage` is left as it was
/// and the partial download removed. `progress` gets the bytes written so
/// far and the total, when GitHub sends one.
pub async fn install(
    tag: &str,
    appimage: &Path,
    mut progress: impl FnMut(u64, Option<u64>),
) -> Result<(), InstallError> {
    let key = PublicKey::decode(RELEASE_KEY).map_err(|err| InstallError::BrokenKey(err.to_string()))?;
    let client = client()?;
    tracing::info!(tag, appimage = %appimage.display(), "installing decanter release");
    let signature = signature(&client, tag).await?;
    let mut verifier = key.verify_stream(&signature).map_err(|err| signature_error(&err))?;
    let Some(mut response) = asset(&client, tag, ASSET).await? else {
        return Err(InstallError::MissingAppImage(tag.to_owned()));
    };
    let total = response.content_length();
    if total.is_some_and(|total| total > MAX_APPIMAGE) {
        return Err(InstallError::TooLarge { limit: MAX_APPIMAGE });
    }
    progress(0, total);

    let dir = appimage.parent().unwrap_or(appimage);
    let partial = dir.join(format!(".{ASSET}.{}.part", std::process::id()));
    let mut options = tokio::fs::OpenOptions::new();
    options.write(true).create_new(true);
    crate::platform::restrict_to_owner(&mut options);
    let file = options
        .open(&partial)
        .await
        .map_err(|err| write_error(&partial, &err))?;

    let written = async {
        let mut writer = tokio::io::BufWriter::with_capacity(1024 * 1024, file);
        let mut done = 0u64;
        let mut reported = 0u64;
        while let Some(chunk) = response.chunk().await.map_err(|err| network_error(&err))? {
            done = done.saturating_add(u64::try_from(chunk.len()).unwrap_or(u64::MAX));
            if done > MAX_APPIMAGE {
                return Err(InstallError::TooLarge { limit: MAX_APPIMAGE });
            }
            verifier.update(&chunk);
            writer
                .write_all(&chunk)
                .await
                .map_err(|err| write_error(&partial, &err))?;
            if done.saturating_sub(reported) >= PROGRESS_STEP {
                reported = done;
                progress(done, total);
            }
        }
        progress(done, total);
        verifier.finalize().map_err(|err| signature_error(&err))?;
        // Only authenticated once the signature verified: a valid signature
        // of an older release must not install as this one.
        let expected = format!("decanter {tag}");
        if signature.trusted_comment() != expected {
            return Err(InstallError::WrongRelease {
                expected,
                found: signature.trusted_comment().to_owned(),
            });
        }
        writer.flush().await.map_err(|err| write_error(&partial, &err))?;
        writer
            .into_inner()
            .sync_all()
            .await
            .map_err(|err| write_error(&partial, &err))?;
        crate::platform::make_executable(&partial)
            .await
            .map_err(|err| write_error(&partial, &err))?;
        tokio::fs::rename(&partial, appimage)
            .await
            .map_err(|err| write_error(appimage, &err))?;
        tracing::info!(tag, bytes = done, "verified release installed over the AppImage");
        Ok(())
    }
    .await;

    if let Err(err) = &written {
        tracing::warn!(tag, error = %err, "release not installed, the AppImage is unchanged");
        if let Err(remove) = tokio::fs::remove_file(&partial).await {
            tracing::warn!(partial = %partial.display(), error = %remove, "cannot remove the partial download");
        }
        return written;
    }
    // The rename only survives a crash once the folder's own entry is on disk.
    match tokio::fs::File::open(dir).await {
        Ok(folder) => {
            if let Err(err) = folder.sync_all().await {
                tracing::warn!(dir = %dir.display(), %err, "cannot sync the AppImage's folder");
            }
        }
        Err(err) => tracing::warn!(dir = %dir.display(), %err, "cannot open the AppImage's folder to sync it"),
    }
    written
}

/// Starts `appimage` in place of this process once decanter has gone
/// through its normal shutdown; see [`restart_if_requested`].
pub fn restart_after_quit(appimage: PathBuf) {
    if RESTART.set(appimage).is_err() {
        tracing::debug!("restart already requested");
    }
}

/// The last thing `main` does: replaces this process with the AppImage a
/// restart was asked for, with the same arguments. Returns only when there
/// was none or it couldn't be started.
pub fn restart_if_requested() {
    let Some(appimage) = RESTART.get() else {
        return;
    };
    let mut command = Command::new(appimage);
    command.args(std::env::args_os().skip(1));
    // The new AppRun sets its own; inheriting ours would stack them.
    decanter_core::strip_launcher_env(&mut command);
    tracing::info!(appimage = %appimage.display(), "restarting into the updated AppImage");
    let err = crate::platform::replace_process(&mut command);
    tracing::error!(appimage = %appimage.display(), %err, "cannot start the updated AppImage");
}
