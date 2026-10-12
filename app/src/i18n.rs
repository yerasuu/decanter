//! The UI language: the one saved in Ajustes, or the system locale's when
//! none is.

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Language {
    Es,
    En,
}

impl Language {
    pub const ALL: [Self; 2] = [Self::Es, Self::En];

    pub const fn code(self) -> &'static str {
        match self {
            Self::Es => "es",
            Self::En => "en",
        }
    }

    pub fn from_code(code: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|language| language.code() == code)
    }

    /// Written in the language itself, never translated, so it can be found
    /// from a UI in a language the reader doesn't know.
    pub const fn native_name(self) -> &'static str {
        match self {
            Self::Es => "Español",
            Self::En => "English",
        }
    }

    /// The first of `LC_ALL`, `LC_MESSAGES` and `LANG` that is set, in
    /// POSIX's order of precedence.
    fn system() -> Self {
        let locale = ["LC_ALL", "LC_MESSAGES", "LANG"]
            .into_iter()
            .filter_map(|name| std::env::var(name).ok())
            .find(|value| !value.is_empty())
            .unwrap_or_default();
        if locale.starts_with("es") { Self::Es } else { Self::En }
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
/// away. Refreshing every window re-renders even the cached views, but text
/// a view already built and kept (an error message, say) stays in the old
/// language until it's built again.
pub fn set(language: Option<Language>, cx: &mut gpui::App) {
    let saved = crate::core::db().and_then(|db| {
        db.set_language(language.map(Language::code))
            .map_err(crate::core::StoreError::Query)
    });
    if let Err(err) = saved {
        tracing::warn!(error = %err, "failed to save language setting");
        return;
    }
    tracing::info!(language = language.map(Language::code), "language setting changed");
    init();
    cx.refresh_windows();
}
