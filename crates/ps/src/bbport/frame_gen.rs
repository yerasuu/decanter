//! The frame generation a game starts with (`PS_FRAME_GEN`), chosen per game because psport.ini is
//! shared by every game.

use super::LauncherSettings;

/// Key under `titles.<TITLE_ID>` in the launch settings.
pub const KEY: &str = "frame_gen";

/// The start of the line psport prints, flushed and on its own, for each change made in the in-game menu.
const MENU_LINE: &str = "Frame generation setting: ";

/// What psport's `frame_gen` takes that decanter offers; `auto` and `dlss` wait for their rows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum FrameGen {
    #[default]
    Off,
    Fsr,
}

impl FrameGen {
    pub const ALL: [Self; 2] = [Self::Off, Self::Fsr];

    pub const fn value(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Fsr => "fsr",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|choice| choice.value() == value)
    }

    /// The choice a menu line reports. Anything else, an unknown value included, reports nothing, since the
    /// line comes from a process the launcher doesn't control.
    pub fn from_menu_line(line: &str) -> Option<Self> {
        let value = line.trim().strip_prefix(MENU_LINE)?.trim();
        let choice = Self::parse(value);
        if choice.is_none() {
            tracing::debug!(
                value = value.escape_default().to_string(),
                "ignored unknown frame generation setting"
            );
        }
        choice
    }

    /// `title_id`'s saved value, checked again since the file can be edited by hand: anything
    /// unknown is off, because a game must stay playable, and is logged.
    pub fn of_title(settings: &LauncherSettings, title_id: &str) -> Self {
        let Some(saved) = settings.title_text(title_id, KEY) else {
            return Self::Off;
        };
        Self::parse(saved).unwrap_or_else(|| {
            tracing::warn!(
                title_id,
                value = saved.escape_default().to_string(),
                "unknown frame_gen, using off"
            );
            Self::Off
        })
    }
}
