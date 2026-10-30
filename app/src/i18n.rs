//! The UI language: the one saved in Ajustes, or the system locale's when
//! none is.

use std::borrow::Cow;
use std::collections::HashSet;
use std::sync::{LazyLock, Mutex};

use decanter_core::capabilities::Feature;
use gpui::AppContext as _;
use rust_i18n::t;

/// Every language with a folder in `locales/`, so adding one is adding its
/// files. English and Spanish lead; the rest follow by native name, so the
/// picker stays in a stable order however many languages are added.
static AVAILABLE: LazyLock<Vec<Language>> = LazyLock::new(|| {
    let mut languages: Vec<Language> = rust_i18n::available_locales!()
        .into_iter()
        // Leaked once into a `'static` so `Language` stays `Copy`.
        .map(|code| Language(Box::leak(code.into_owned().into_boxed_str())))
        .collect();
    let rank = |language: &Language| {
        LEADING
            .iter()
            .position(|code| *code == language.0)
            .unwrap_or(LEADING.len())
    };
    languages.sort_by_cached_key(|language| (rank(language), language.native_name().to_lowercase()));
    languages
});

const LEADING: [&str; 2] = ["en", "es"];

/// What a system locale none of `AVAILABLE` matches gets.
const OTHER_SYSTEMS: &str = "en";

const MACHINE_NOTE_KEY: &str = "language.machine_note";

#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Language(&'static str);

impl Language {
    pub fn all() -> impl Iterator<Item = Self> {
        AVAILABLE.iter().copied()
    }

    pub const fn code(self) -> &'static str {
        self.0
    }

    pub fn from_code(code: &str) -> Option<Self> {
        Self::all().find(|language| language.0 == code)
    }

    /// Written in the language itself, never translated, so it can be found
    /// from a UI in a language the reader doesn't know.
    pub fn native_name(self) -> Cow<'static, str> {
        t!("language.native_name", locale = self.0)
    }

    /// A sentence in this language saying its translation is automatic, for
    /// the languages whose `_language.<code>.yml` defines it; reviewed ones
    /// (en, es) have none. Looked up in the language alone, never through the
    /// fallback, so a language without the key can't show another's note.
    pub fn machine_note(self) -> Option<Cow<'static, str>> {
        crate::_rust_i18n_backend()
            .translate(self.0, MACHINE_NOTE_KEY)
            .map(|note| note.into_owned().into())
    }

    /// The system locale's language, matched by region first (`pt_BR` as
    /// `pt-BR`), then by language, then by any folder of the same language
    /// (`pt_PT` and plain `pt` get `pt-BR`, `zh_Hans_CN` gets `zh-CN`).
    /// Traditional Chinese (`zh_TW`, `zh_HK`, `zh_MO`, any `Hant`) gets
    /// English instead: the only Chinese on offer is Simplified, and
    /// silently showing it to readers of the other script would be wrong.
    pub fn system() -> Self {
        let locale = decanter_core::platform::system_locale().unwrap_or_default();
        let tag = locale.split(['.', '@']).next().unwrap_or_default().replace('_', "-");
        Self::from_system_tag(&tag)
    }

    fn from_system_tag(tag: &str) -> Self {
        let mut subtags = tag.split('-');
        let primary = subtags.next().unwrap_or_default();
        let traditional = primary.eq_ignore_ascii_case("zh")
            && subtags.any(|subtag| {
                ["Hant", "TW", "HK", "MO"]
                    .iter()
                    .any(|marker| subtag.eq_ignore_ascii_case(marker))
            });
        let same_language = |language: &Language| {
            language
                .0
                .split('-')
                .next()
                .is_some_and(|code| code.eq_ignore_ascii_case(primary))
        };
        if traditional {
            return Self(OTHER_SYSTEMS);
        }
        Self::all()
            .find(|language| language.0.eq_ignore_ascii_case(tag))
            .or_else(|| Self::all().find(|language| language.0.eq_ignore_ascii_case(primary)))
            .or_else(|| Self::all().find(same_language))
            .unwrap_or(Self(OTHER_SYSTEMS))
    }
}

/// "Language" in every language, the current one first, for the first-run
/// screen: whoever reads it may not know the one it started in.
pub fn label_in_all() -> String {
    let current = rust_i18n::locale();
    let is_current = |language: &Language| language.code() == &*current;
    Language::all()
        .filter(is_current)
        .chain(Language::all().filter(|language| !is_current(language)))
        .map(|language| t!("language.label", locale = language.code()))
        .collect::<Vec<_>>()
        .join(" · ")
}

/// UI text kept to be shown later: its key and arguments, translated each time
/// it's shown, so a view that holds one follows a language switch instead of
/// keeping the words of the language it was built in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Text {
    key: &'static str,
    args: Vec<(&'static str, Arg)>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Arg {
    Verbatim(String),
    Text(Text),
}

impl Text {
    pub const fn new(key: &'static str) -> Self {
        Self { key, args: Vec::new() }
    }

    /// Nothing but `value`, the same in every language: an error worded
    /// outside decanter, where a translated one is expected.
    pub fn verbatim(value: impl std::fmt::Display) -> Self {
        Self::new("common.errors.verbatim").arg("error", value)
    }

    /// Whether this text, or one it wraps, is `key`: how a view tells which
    /// failure a stored error is, to offer its way out.
    #[must_use]
    pub fn is_or_wraps(&self, key: &str) -> bool {
        self.key == key
            || self.args.iter().any(|(_, arg)| match arg {
                Arg::Text(text) => text.is_or_wraps(key),
                Arg::Verbatim(_) => false,
            })
    }

    /// `value` as it is in every language: a path, a number, a name, an error
    /// worded outside decanter.
    #[must_use]
    pub fn arg(mut self, name: &'static str, value: impl std::fmt::Display) -> Self {
        self.args.push((name, Arg::Verbatim(value.to_string())));
        self
    }

    /// `value` translated along with this text.
    #[must_use]
    pub fn text(mut self, name: &'static str, value: Self) -> Self {
        self.args.push((name, Arg::Text(value)));
        self
    }
}

impl std::fmt::Display for Text {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        note_if_missing(self.key);
        let template = t!(self.key);
        if self.args.is_empty() {
            return f.write_str(&template);
        }
        let mut names = Vec::with_capacity(self.args.len());
        let mut values = Vec::with_capacity(self.args.len());
        for (name, arg) in &self.args {
            names.push(*name);
            values.push(match arg {
                Arg::Verbatim(value) => value.clone(),
                Arg::Text(text) => text.to_string(),
            });
        }
        f.write_str(&rust_i18n::replace_patterns(&template, &names, &values))
    }
}

impl From<&Text> for gpui::SharedString {
    fn from(text: &Text) -> Self {
        text.to_string().into()
    }
}

impl From<Text> for gpui::SharedString {
    fn from(text: Text) -> Self {
        (&text).into()
    }
}

/// Logs, once per language and key, a key the current language lacks and
/// English or Spanish is standing in for, so an incomplete translation shows
/// up in the logs instead of as unexplained foreign text. Only the keys
/// that go through [`Text`] are seen; a direct `t!` falls back silently.
fn note_if_missing(key: &'static str) {
    static SEEN: LazyLock<Mutex<HashSet<(String, &'static str)>>> = LazyLock::new(Mutex::default);
    if !tracing::enabled!(tracing::Level::DEBUG) {
        return;
    }
    let locale = rust_i18n::locale();
    if crate::_rust_i18n_backend().translate(&locale, key).is_some() {
        return;
    }
    let first_time = SEEN.lock().is_ok_and(|mut seen| seen.insert((locale.to_string(), key)));
    if first_time {
        tracing::debug!(
            language = &*locale,
            key,
            "translation missing, using the fallback language"
        );
    }
}

pub fn saved() -> Option<Language> {
    crate::core::db()
        .and_then(|db| db.language().map_err(crate::core::StoreError::Query))
        .inspect_err(|err| tracing::warn!(error = %err, "failed to read language setting, following the system"))
        .ok()
        .flatten()
        .and_then(|code| Language::from_code(&code))
}

pub fn init() {
    let language = saved().unwrap_or_else(Language::system);
    rust_i18n::set_locale(language.code());
    tracing::debug!(language = language.code(), "resolved UI language");
}

/// Saves `language` (`None` follows the system) and switches to it right
/// away, bbport's in-game menu included from its next run. Refreshing every
/// window re-renders even the cached views, and text a view keeps is a
/// [`Text`], translated as it's drawn; only the lines already in a job's log
/// stay in the language they were written in.
///
/// Switches even when it can't be saved, since the first-run screen picks a
/// language before any library holds a database; that screen saves it again
/// once there is one.
pub fn set(language: Option<Language>, cx: &mut gpui::App) {
    let saved = crate::core::db().and_then(|db| {
        db.set_language(language.map(Language::code))
            .map_err(crate::core::StoreError::Query)
    });
    if let Err(err) = saved {
        tracing::warn!(error = %err, "failed to save language setting, switching for this session only");
    }
    tracing::info!(language = language.map(Language::code), "language setting changed");
    rust_i18n::set_locale(language.unwrap_or_else(Language::system).code());
    crate::platform::refresh_tray();
    if crate::capabilities::Caps::get(cx).is_offered(Feature::PsSection) {
        cx.background_spawn(async {
            let root = decanter_ps::paths::active();
            if let Err(err) = decanter_ps::bbport::Bbport::forget_menu_language(&root) {
                tracing::warn!(error = %err, "could not drop the in-game menu's language from every port ini");
            }
        })
        .detach();
    }
    cx.refresh_windows();
}
