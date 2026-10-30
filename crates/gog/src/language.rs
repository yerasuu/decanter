// SPDX-License-Identifier: GPL-3.0-or-later
//! The languages GOG splits a build's depots by, and which of them a game is installed in.
//!
//! A depot of a v2 build lists its languages as tags (`en-US`, `zh-Hans`), or `*` for files every
//! language needs. GOG documents none of it publicly; the shape is the one the builds endpoint
//! answers (see `docs/gog-api-behaviour-spec.md`). A tag missing from [`KNOWN`] is kept verbatim
//! and shown as it is, since a name made up for it could be wrong.

/// One language GOG tags depots with: its tag, the English name GOG Galaxy writes into a game's
/// `goggame-<id>.info`, and its name in the language itself for the pickers.
struct Known {
    tag: &'static str,
    english: &'static str,
    native: &'static str,
}

const fn known(tag: &'static str, english: &'static str, native: &'static str) -> Known {
    Known { tag, english, native }
}

#[rustfmt::skip]
const KNOWN: &[Known] = &[
    known("en-US", "English", "English"),
    known("en-GB", "British English", "British English"),
    known("es-ES", "Spanish", "Español (España)"),
    known("es-MX", "Latin American Spanish", "Español (Latinoamérica)"),
    known("de-DE", "German", "Deutsch"),
    known("fr-FR", "French", "Français"),
    known("it-IT", "Italian", "Italiano"),
    known("pt-BR", "Brazilian Portuguese", "Português (Brasil)"),
    known("pt-PT", "Portuguese", "Português (Portugal)"),
    known("ru-RU", "Russian", "Русский"),
    known("pl-PL", "Polish", "Polski"),
    known("cs-CZ", "Czech", "Čeština"),
    known("hu-HU", "Hungarian", "Magyar"),
    known("tr-TR", "Turkish", "Türkçe"),
    known("uk-UA", "Ukrainian", "Українська"),
    known("nl-NL", "Dutch", "Nederlands"),
    known("sv-SE", "Swedish", "Svenska"),
    known("da-DK", "Danish", "Dansk"),
    known("fi-FI", "Finnish", "Suomi"),
    known("nb-NO", "Norwegian", "Norsk"),
    known("ro-RO", "Romanian", "Română"),
    known("el-GR", "Greek", "Ελληνικά"),
    known("bg-BG", "Bulgarian", "Български"),
    known("sk-SK", "Slovak", "Slovenčina"),
    known("ar", "Arabic", "العربية"),
    known("he-IL", "Hebrew", "עברית"),
    known("th-TH", "Thai", "ไทย"),
    known("vi-VN", "Vietnamese", "Tiếng Việt"),
    known("id-ID", "Indonesian", "Bahasa Indonesia"),
    known("ja-JP", "Japanese", "日本語"),
    known("ko-KR", "Korean", "한국어"),
    known("zh-Hans", "Simplified Chinese", "简体中文"),
    known("zh-Hant", "Traditional Chinese", "繁體中文"),
];

/// The tag a game falls back to when it doesn't offer the one asked for: GOG's own default.
pub const FALLBACK: &str = "en-US";

/// What a depot's `languages` entry means "files every language needs".
const ANY: &str = "*";

/// The GOG tags that fit one of decanter's UI languages, best first: `es` reads `es-ES`, then
/// the Latin American `es-MX`. Anything unknown reads as [`FALLBACK`].
#[must_use]
#[inline]
pub fn preferred_for_ui(ui_code: &str) -> &'static [&'static str] {
    match ui_code {
        "es" => &["es-ES", "es-MX"],
        "it" => &["it-IT"],
        "pt-BR" => &["pt-BR", "pt-PT"],
        "de" => &["de-DE"],
        "fr" => &["fr-FR"],
        "ru" => &["ru-RU"],
        "zh-CN" => &["zh-Hans"],
        "ja" => &["ja-JP"],
        "ko" => &["ko-KR"],
        _ => &[FALLBACK, "en-GB"],
    }
}

fn lookup(tag: &str) -> Option<&'static Known> {
    KNOWN
        .iter()
        .find(|known| known.tag.eq_ignore_ascii_case(tag) || known.english.eq_ignore_ascii_case(tag))
}

/// `tag` as [`KNOWN`] spells it, also when a depot names the language in English rather than by
/// tag; anything else as it came.
#[must_use]
#[inline]
pub fn canonical(tag: &str) -> String {
    lookup(tag).map_or_else(|| tag.trim().to_owned(), |known| known.tag.to_owned())
}

/// The English name GOG Galaxy writes as a game's `language`; the tag itself when unknown.
#[must_use]
#[inline]
pub fn english_name(tag: &str) -> String {
    lookup(tag).map_or_else(|| tag.to_owned(), |known| known.english.to_owned())
}

/// The language's name in itself; the tag itself when unknown.
#[must_use]
#[inline]
pub fn native_name(tag: &str) -> String {
    lookup(tag).map_or_else(|| tag.to_owned(), |known| known.native.to_owned())
}

/// Whether a depot's `languages` say it holds files for every language: no list, an empty one,
/// or `*` among them.
pub(crate) fn is_neutral(languages: Option<&[String]>) -> bool {
    languages.is_none_or(|tags| tags.is_empty() || tags.iter().any(|tag| tag.trim() == ANY))
}

/// The language and script part of a tag, lowercased: `es` for `es-MX`, but `zh-hans` and
/// `zh-hant` apart, since Simplified and Traditional Chinese are not one language to a reader.
fn family(tag: &str) -> String {
    let lower = tag.to_ascii_lowercase();
    let mut subtags = lower.split(['-', '_']);
    let primary = subtags.next().unwrap_or_default().to_owned();
    if primary != "zh" {
        return primary;
    }
    match subtags.next() {
        Some("hans" | "cn" | "sg") => "zh-hans".to_owned(),
        Some("hant" | "tw" | "hk" | "mo") => "zh-hant".to_owned(),
        _ => primary,
    }
}

/// Which of `offered` a game asked for `wanted` (best first) is installed in.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Resolved {
    pub tag: String,
    /// False when none of `wanted` is offered and `tag` is the fallback.
    pub is_wanted: bool,
}

impl Resolved {
    /// A language the user picked from those offered.
    #[must_use]
    #[inline]
    pub const fn picked(tag: String) -> Self {
        Self { tag, is_wanted: true }
    }
}

/// The tag of `offered` that fits `wanted` best: the same tag, else the same language in another
/// region, else [`FALLBACK`] or another English, else the first offered. `None` only when
/// `offered` is empty.
#[must_use]
#[inline]
pub fn resolve(wanted: &[String], offered: &[String]) -> Option<Resolved> {
    let exact = wanted.iter().find_map(|tag| {
        let tag = canonical(tag);
        offered.iter().find(|offer| offer.eq_ignore_ascii_case(&tag))
    });
    let same_family = || {
        wanted.iter().find_map(|tag| {
            let family = family(&canonical(tag));
            offered.iter().find(|offer| self::family(offer) == family)
        })
    };
    if let Some(tag) = exact.or_else(same_family) {
        return Some(Resolved {
            tag: tag.clone(),
            is_wanted: true,
        });
    }
    let fallback = offered
        .iter()
        .find(|offer| offer.eq_ignore_ascii_case(FALLBACK))
        .or_else(|| offered.iter().find(|offer| family(offer) == "en"))
        .or_else(|| offered.first());
    fallback.map(|tag| Resolved {
        tag: tag.clone(),
        is_wanted: false,
    })
}

/// Whether a language-specific depot listing `languages` holds `tag`'s files.
pub(crate) fn holds(languages: &[String], tag: &str) -> bool {
    languages
        .iter()
        .any(|language| canonical(language).eq_ignore_ascii_case(tag))
}
