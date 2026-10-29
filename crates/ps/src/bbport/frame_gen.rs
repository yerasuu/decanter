//! The frame generation a game starts with (`PS_FRAME_GEN`), chosen per game because psport.ini is
//! shared by every game.

use super::LauncherSettings;

/// Key under `titles.<TITLE_ID>` in the launch settings.
pub const KEY: &str = "frame_gen";

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
