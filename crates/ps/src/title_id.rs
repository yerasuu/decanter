//! A title's id as decanter names a folder of its own settings after it.

use core::str::FromStr;

/// `CUSA03173`, `PPSA02929`: four capital letters and five digits. It comes from a dump's
/// `param.sfo` or `param.json`, so it is checked here before it names a folder.
pub struct TitleId(String);

#[derive(Debug, thiserror::Error)]
#[error("not a title id: {0:?}")]
pub struct InvalidTitleId(String);

impl TitleId {
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl FromStr for TitleId {
    type Err = InvalidTitleId;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let valid = text.len() == 9
            && text.split_at_checked(4).is_some_and(|(letters, digits)| {
                letters.bytes().all(|byte| byte.is_ascii_uppercase())
                    && digits.bytes().all(|byte| byte.is_ascii_digit())
            });
        if valid {
            Ok(Self(text.to_owned()))
        } else {
            Err(InvalidTitleId(text.to_owned()))
        }
    }
}
