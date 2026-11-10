//! Who made a downloaded mod and on what terms, kept in its `mod.toml` `[origin]` so the Mods tab
//! credits the authors and the platform offline (docs/mod-platforms-attribution.md, checklist item
//! 7). A platform's crate fills it from its answer at install and again on an update check; it is
//! checked here, when it is attached and when it is read, since it is shown and its links opened.

use std::time::{SystemTime, UNIX_EPOCH};

use crate::manifest::ModManifest;
use crate::path::ModId;
use crate::{Error, GameMods, Result};

const MAX_AUTHOR_CHARS: usize = 200;
/// Enough for a permissions paragraph; a longer one is cut and its page has the rest.
const MAX_LICENCE_CHARS: usize = 2000;
const MAX_URL_BYTES: usize = 2048;
const MAX_PERMISSIONS: usize = 32;
const MAX_PERMISSION_BYTES: usize = 64;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct ModCredits {
    pub author: Option<String>,
    /// The author's profile, on the platform's own hosts.
    pub author_url: Option<String>,
    /// The mod's page, on the platform's own hosts.
    pub page_url: Option<String>,
    /// A site the author lists for the mod, on any HTTPS host.
    pub website_url: Option<String>,
    /// The licence or permissions as the platform words them, plain text.
    pub licence: Option<String>,
    /// Tokens the app names, such as mod.io's decoded `credit_options`.
    pub permissions: Vec<String>,
    pub is_adult: bool,
    /// Unix seconds when the platform was asked.
    pub fetched_at: i64,
}

impl ModCredits {
    /// Empty credits stamped with the current time, for a platform's crate to fill.
    #[must_use]
    #[inline]
    pub fn fetched_now() -> Self {
        Self {
            fetched_at: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |since| i64::try_from(since.as_secs()).unwrap_or(i64::MAX)),
            ..Self::default()
        }
    }

    /// The credits with every text made plain and bounded, and every link or token `platform` may
    /// not carry dropped, since a platform's answer is never trusted to keep the rules. What is
    /// dropped is logged at debug level only, because a browser checks what it shows every frame.
    /// A platform's crate checks what it shows before an install; attaching them to an origin
    /// checks again, which changes nothing in credits already checked.
    #[must_use]
    #[inline]
    pub fn checked(mut self, platform: &str) -> Self {
        self.author = self
            .author
            .as_deref()
            .map(|text| plain(text, MAX_AUTHOR_CHARS))
            .filter(|text| !text.is_empty());
        self.licence = self
            .licence
            .as_deref()
            .map(|text| plain(text, MAX_LICENCE_CHARS))
            .filter(|text| !text.is_empty());
        let domains = platform_domains(platform);
        for (field, url, allowed) in [
            ("author_url", &mut self.author_url, Some(domains)),
            ("page_url", &mut self.page_url, Some(domains)),
            ("website_url", &mut self.website_url, None),
        ] {
            if url.as_deref().is_some_and(|given| !is_link(given, allowed)) {
                tracing::debug!(platform, field, "mod credit link refused, left out");
                *url = None;
            }
        }
        let given = self.permissions.len();
        let mut permissions = Vec::with_capacity(given.min(MAX_PERMISSIONS));
        for token in self.permissions {
            if is_permission(&token) && !permissions.contains(&token) && permissions.len() < MAX_PERMISSIONS {
                permissions.push(token);
            }
        }
        if permissions.len() < given {
            tracing::debug!(
                platform,
                given,
                kept = permissions.len(),
                "mod credit permissions refused, left out"
            );
        }
        self.permissions = permissions;
        self.fetched_at = self.fetched_at.max(0);
        self
    }

    /// What a `mod.toml` must hold to be read back: what [`Self::checked`] would leave as it is.
    pub(crate) fn check(&self, platform: &str) -> core::result::Result<(), &'static str> {
        let is_plain =
            |text: Option<&str>, max: usize| text.is_none_or(|text| !text.is_empty() && plain(text, max) == text);
        if !is_plain(self.author.as_deref(), MAX_AUTHOR_CHARS) {
            return Err("author is too long or has control characters");
        }
        if !is_plain(self.licence.as_deref(), MAX_LICENCE_CHARS) {
            return Err("licence is too long or has control characters");
        }
        let domains = platform_domains(platform);
        if [&self.author_url, &self.page_url]
            .into_iter()
            .any(|url| url.as_deref().is_some_and(|given| !is_link(given, Some(domains))))
        {
            return Err("a credit link is not HTTPS on the platform's hosts");
        }
        if self.website_url.as_deref().is_some_and(|given| !is_link(given, None)) {
            return Err("website_url is not an HTTPS link");
        }
        if self.permissions.len() > MAX_PERMISSIONS || !self.permissions.iter().all(|token| is_permission(token)) {
            return Err("a permission is not a short token");
        }
        if self.fetched_at < 0 {
            return Err("fetched_at is before 1970");
        }
        Ok(())
    }
}

impl GameMods {
    /// Replaces what mod `id`'s origin credits with what its platform says now, as an update check
    /// reads it again.
    ///
    /// # Errors
    ///
    /// [`Error::UnknownMod`] for a mod the game does not have, [`Error::InvalidOrigin`] for one that
    /// did not come from a platform, else when its `mod.toml` can't be read or written.
    #[inline]
    pub fn set_credits(&self, id: &ModId, credits: ModCredits) -> Result<()> {
        let dir = self.mod_dir(id);
        let mut manifest = ModManifest::read(&dir, id)?.ok_or_else(|| Error::UnknownMod(id.to_string()))?;
        let Some(origin) = manifest.origin.take() else {
            return Err(Error::InvalidOrigin(id.to_string()));
        };
        let refreshed = origin.with_credits(credits);
        let platform = refreshed.platform.clone();
        manifest.origin = Some(refreshed);
        manifest.write(&dir)?;
        tracing::info!(mod_id = %id, platform, "mod credits refreshed");
        Ok(())
    }
}

/// The hosts, and their subdomains, a platform's own links may point to.
fn platform_domains(platform: &str) -> &'static [&'static str] {
    match platform {
        "thunderstore" => &["thunderstore.io"],
        "modio" => &["mod.io"],
        "nexus" => &["nexusmods.com"],
        _ => &[],
    }
}

/// Control characters as spaces, trimmed, at most `max` characters; applying it twice changes
/// nothing, which reading a `mod.toml` back relies on.
fn plain(text: &str, max: usize) -> String {
    let spaced: String = text
        .chars()
        .map(|character| if character.is_control() { ' ' } else { character })
        .collect();
    spaced
        .trim()
        .chars()
        .take(max)
        .collect::<String>()
        .trim_end()
        .to_owned()
}

fn is_permission(token: &str) -> bool {
    (1..=MAX_PERMISSION_BYTES).contains(&token.len())
        && token
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

/// An HTTPS link with a plain host name (no credentials, no port, nothing a browser could read
/// another host out of), under one of `domains` when given. Checked by hand rather than parsed,
/// since this crate has no URL parser and a stricter rule than a parser's is enough for links an
/// API gives.
fn is_link(text: &str, domains: Option<&[&str]>) -> bool {
    if text.len() > MAX_URL_BYTES || !text.bytes().all(|byte| byte.is_ascii_graphic()) {
        return false;
    }
    let Some(rest) = text
        .get(..8)
        .filter(|scheme| scheme.eq_ignore_ascii_case("https://"))
        .and_then(|_| text.get(8..))
    else {
        return false;
    };
    let host = rest.split(['/', '?', '#']).next().unwrap_or_default();
    let is_host = host.contains('.')
        && !host.starts_with(['.', '-'])
        && !host.ends_with(['.', '-'])
        && !host.contains("..")
        && host
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-'));
    is_host && domains.is_none_or(|domains| domains.iter().any(|domain| is_under(host, domain)))
}

fn is_under(host: &str, domain: &str) -> bool {
    host.eq_ignore_ascii_case(domain)
        || host
            .len()
            .checked_sub(domain.len().saturating_add(1))
            .and_then(|cut| host.get(cut..))
            .is_some_and(|tail| {
                tail.starts_with('.') && tail.get(1..).is_some_and(|rest| rest.eq_ignore_ascii_case(domain))
            })
}
