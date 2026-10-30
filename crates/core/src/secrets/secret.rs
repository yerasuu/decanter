use core::fmt;

use zeroize::Zeroizing;

/// A credential's value: a GOG session, a shadNet password or token, an API key.
///
/// Zeroed when dropped. Its `Debug` prints no part of it, and it has no `Display`, `Clone` or
/// `Serialize`, so a struct holding one can derive `Debug` without a log ever carrying the value;
/// [`Secret::expose`] is the only way to the text, spelled out where it is sent.
pub struct Secret(Zeroizing<String>);

impl Secret {
    #[inline]
    #[must_use]
    pub fn expose(&self) -> &str {
        &self.0
    }

    #[inline]
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl From<String> for Secret {
    #[inline]
    fn from(value: String) -> Self {
        Self(Zeroizing::new(value))
    }
}

impl fmt::Debug for Secret {
    #[inline]
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(<redacted>)")
    }
}
