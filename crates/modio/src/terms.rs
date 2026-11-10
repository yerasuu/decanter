// SPDX-License-Identifier: GPL-3.0-or-later
//! mod.io's terms of use, which the user accepts in decanter before anything else is asked
//! (Terms and user consent page, analysis section 3).
//!
//! `GET /authenticate/terms` gives the text, in mod.io's own words and buttons, with the links to
//! the Terms of Use and the Privacy Policy the dialog must show. Only the user's own press records
//! an agreement: the SHA-256 of the text agreed to, in `<root>/decanter/modio/terms.toml`, so that
//! changed terms are shown again. A 403 with error 11074 on any request drops the agreement, since
//! mod.io says then that it wants them accepted anew. `terms_agreed` itself is a parameter of
//! mod.io's login calls, which this crate does not make: it never sends one.

use std::fs;
use std::io::{self, Read as _};
use std::path::Path;

use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

use crate::http::Client;
use crate::id::{checked_url, display_text, now_unix};
use crate::{Error, Result};

const CALL: &str = "the terms of use";
const FILE: &str = "terms.toml";
const SCHEMA: u32 = 1;
const MAX_BYTES: u64 = 1 << 20;
const MAX_FILE_BYTES: u64 = 4 << 10;
const MAX_TEXT_CHARS: usize = 20_000;
const MAX_BUTTON_CHARS: usize = 60;
/// The pages the analysis names; used when the answer does not give its own, and linked from
/// Ajustes beside the key.
pub const TERMS_PAGE: &str = "https://mod.io/legal/terms";
pub const PRIVACY_PAGE: &str = "https://mod.io/legal/privacy";

#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct Terms {
    /// Plain text, shown as it is.
    pub text: String,
    pub agree: String,
    pub disagree: String,
    pub terms_url: String,
    pub privacy_url: String,
    /// The SHA-256 of mod.io's text, which an agreement is recorded against.
    pub digest: String,
}

#[derive(Deserialize)]
struct RawTerms {
    plaintext: String,
    #[serde(default)]
    buttons: Option<RawButtons>,
    #[serde(default)]
    links: Option<RawLinks>,
}

#[derive(Deserialize)]
struct RawButtons {
    #[serde(default)]
    agree: Option<RawText>,
    #[serde(default)]
    disagree: Option<RawText>,
}

#[derive(Deserialize)]
struct RawText {
    #[serde(default)]
    text: Option<String>,
}

#[derive(Deserialize)]
struct RawLinks {
    #[serde(default)]
    terms: Option<RawLink>,
    #[serde(default)]
    privacy: Option<RawLink>,
}

#[derive(Deserialize)]
struct RawLink {
    #[serde(default)]
    url: Option<String>,
}

#[derive(Serialize, Deserialize)]
struct Agreement {
    schema: u32,
    digest: String,
    agreed_at: i64,
}

/// The terms as mod.io words them now.
///
/// # Errors
///
/// As a request, or [`Error::Malformed`] when the answer has no text.
#[inline]
pub async fn terms(client: &Client) -> Result<Terms> {
    let raw: RawTerms = client.api("/authenticate/terms", &[], CALL, MAX_BYTES).await?;
    // Line breaks are the text's paragraphs, kept apart from the other control characters.
    let text = raw
        .plaintext
        .lines()
        .map(|line| display_text(line, MAX_TEXT_CHARS))
        .collect::<Vec<_>>()
        .join("\n")
        .chars()
        .take(MAX_TEXT_CHARS)
        .collect::<String>();
    if text.trim().is_empty() {
        return Err(Error::malformed(CALL, "no text"));
    }
    let button = |pick: fn(&RawButtons) -> Option<&RawText>| {
        raw.buttons
            .as_ref()
            .and_then(pick)
            .and_then(|button| button.text.as_deref())
            .map(|label| display_text(label, MAX_BUTTON_CHARS))
            .filter(|label| !label.is_empty())
    };
    let link = |pick: fn(&RawLinks) -> Option<&RawLink>, fallback: &str| {
        raw.links
            .as_ref()
            .and_then(pick)
            .and_then(|link| link.url.as_deref())
            .and_then(checked_url)
            .map_or_else(|| fallback.to_owned(), String::from)
    };
    let digest = hex(&Sha256::digest(raw.plaintext.as_bytes()));
    Ok(Terms {
        agree: button(|buttons| buttons.agree.as_ref()).unwrap_or_default(),
        disagree: button(|buttons| buttons.disagree.as_ref()).unwrap_or_default(),
        terms_url: link(|links| links.terms.as_ref(), TERMS_PAGE),
        privacy_url: link(|links| links.privacy.as_ref(), PRIVACY_PAGE),
        text,
        digest,
    })
}

/// Whether the user agreed in decanter to these very terms.
///
/// # Errors
///
/// [`Error::Io`] when the file is there and can't be read; one that does not hold up is no
/// agreement.
#[inline]
pub fn agreed(dir: &Path, terms: &Terms) -> Result<bool> {
    let path = dir.join(FILE);
    let file = match fs::File::open(&path) {
        Ok(file) => file,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(err) => return Err(Error::io(&path)(err)),
    };
    let mut text = String::new();
    match file.take(MAX_FILE_BYTES).read_to_string(&mut text) {
        Ok(_) => {}
        Err(err) if err.kind() == io::ErrorKind::InvalidData => return Ok(false),
        Err(err) => return Err(Error::io(&path)(err)),
    }
    match toml::from_str::<Agreement>(decanter_core::bom::strip_str(&text, &path)) {
        Ok(agreement) => Ok(agreement.schema == SCHEMA && agreement.digest == terms.digest),
        Err(err) => {
            tracing::warn!(path = %path.display(), %err, "modio terms agreement unreadable, asking again");
            Ok(false)
        }
    }
}

/// Records that the user pressed agree on `terms`. Only a user's press may call this.
///
/// # Errors
///
/// [`Error::Io`] or [`Error::Settings`] when the file can't be written; it is replaced in one rename.
#[inline]
pub fn record_agreement(dir: &Path, terms: &Terms) -> Result<()> {
    let path = dir.join(FILE);
    let text = toml::to_string(&Agreement {
        schema: SCHEMA,
        digest: terms.digest.clone(),
        agreed_at: now_unix(),
    })
    .map_err(|err| Error::Settings {
        path: path.clone(),
        reason: err.to_string(),
    })?;
    write_replacing(dir, &path, &text)?;
    tracing::info!("modio terms of use agreed by the user");
    Ok(())
}

/// Drops the agreement, when mod.io asks for the terms again (error 11074).
///
/// # Errors
///
/// [`Error::Io`] when the file is there and can't be removed.
#[inline]
pub fn forget_agreement(dir: &Path) -> Result<()> {
    let path = dir.join(FILE);
    match fs::remove_file(&path) {
        Ok(()) => {
            tracing::info!("modio terms agreement dropped, mod.io asked for the terms again");
            Ok(())
        }
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(Error::io(&path)(err)),
    }
}

pub(crate) fn write_replacing(dir: &Path, path: &Path, text: &str) -> Result<()> {
    fs::create_dir_all(dir).map_err(Error::io(dir))?;
    let partial = path.with_extension(format!("toml.{}.tmp", std::process::id()));
    fs::write(&partial, text).map_err(Error::io(&partial))?;
    fs::rename(&partial, path).map_err(|err| {
        if let Err(cleanup) = fs::remove_file(&partial)
            && cleanup.kind() != io::ErrorKind::NotFound
        {
            tracing::warn!(path = %partial.display(), %cleanup, "could not remove a partial file");
        }
        Error::io(path)(err)
    })
}

pub(crate) fn hex(digest: &[u8]) -> String {
    let mut text = String::with_capacity(digest.len().saturating_mul(2));
    for byte in digest {
        for nibble in [byte >> 4_u8, byte & 0x0f] {
            text.push(char::from_digit(u32::from(nibble), 16).unwrap_or('0'));
        }
    }
    text
}
