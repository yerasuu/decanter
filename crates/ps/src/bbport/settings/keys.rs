//! Which of a game's settings go together ([`Category`], one per tab of its page) and what each may
//! hold, for what brings settings in from outside: a file or the command line. The launch keys are
//! an allow-list, so a key nobody named here is refused; the port ini's keys follow the port's own
//! shape, since the port lists a title's effect keys itself, and those decanter knows are typed.

use std::path::{Component, Path};

use serde_json::Value;

use super::{LauncherSettings, SYS_MODULES, title};
use crate::bbport::frame_gen::{self, FrameGen};
use crate::bbport::local_player::{self, LocalPlayer};
use crate::bbport::online::{self, LocalServer};
use crate::bbport::port_ini::MENU_LANGUAGE;
use crate::bbport::{Control, GROUPS, Setting, Store, mods};

const MAX_INI_KEY: usize = 64;
const MAX_INI_VALUE: usize = 512;
const MAX_NAME: usize = 128;
const MAX_WORD: usize = 32;
const MAX_ENV: usize = 4096;
const MAX_ENV_NAME: usize = 128;
const MAX_GUID: usize = 64;

/// A part of a game's settings that can be copied on its own.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Category {
    /// The Graphics tab: display, upscaling, effects, mouse, keyboard-as-stick, performance and
    /// development rows, frame generation.
    Graphics,
    /// The keyboard bindings (`key.*`).
    Keyboard,
    /// The pad layout (`pad.*`), the controller picked and the button icons.
    Gamepad,
    /// Whether mods load, the mods and patches folders, the load order and the patch switches.
    Mods,
    /// Online play and its options, and the local player.
    Online,
    /// The extra environment and whether the system modules dump loads.
    Extras,
}

impl Category {
    pub const ALL: [Self; 6] = [
        Self::Graphics,
        Self::Keyboard,
        Self::Gamepad,
        Self::Mods,
        Self::Online,
        Self::Extras,
    ];

    /// As the command line and the UI name it.
    #[must_use]
    pub const fn id(self) -> &'static str {
        match self {
            Self::Graphics => "graphics",
            Self::Keyboard => "keyboard",
            Self::Gamepad => "gamepad",
            Self::Mods => "mods",
            Self::Online => "online",
            Self::Extras => "extras",
        }
    }

    #[must_use]
    pub fn parse(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|category| category.id() == id)
    }
}

/// Why a value can't be one of a game's settings. A value is never repeated back, since a hand-made
/// file may carry anything.
#[derive(Debug, thiserror::Error)]
pub enum Refusal {
    #[error("not a settings file exported by decanter")]
    NotExport,
    #[error("settings file format {0} is newer than this decanter reads")]
    Version(u64),
    #[error("the settings file is larger than {0} bytes")]
    TooLarge(u64),
    #[error("{0} isn't a title id")]
    TitleId(String),
    #[error("{0} isn't a setting a game owns")]
    UnknownKey(String),
    #[error("{0} is shared by every game, not one game's")]
    SharedKey(String),
    #[error("{0} can't take that value")]
    Value(String),
    #[error("{0} names a folder outside the library")]
    Outside(String),
    #[error("the settings file has too many entries")]
    TooMany,
    #[error("a mod or patch name isn't one a folder may have")]
    Name,
}

/// What a launch key may hold.
enum Kind {
    Flag,
    Number {
        min: f64,
        max: f64,
    },
    Choice(&'static [(&'static str, &'static str, Option<&'static str>)]),
    FrameGen,
    LocalServer,
    LocalPlayer,
    /// A controller's GUID, or empty for the automatic choice.
    Gamepad,
    /// A controller's name, shown while it is unplugged.
    Name,
    /// An identifier such as an icon style.
    Word,
    /// `NAME=value` items, as `extra_env` is passed at launch.
    Env,
    /// A mods or patches folder.
    Folder,
}

fn rows() -> impl Iterator<Item = &'static Setting> {
    GROUPS.iter().flat_map(|group| group.settings)
}

fn launch_kind(key: &str) -> Option<Kind> {
    if let Some(setting) = rows().find(|setting| setting.store == Store::Launcher && setting.key == key) {
        return Some(match setting.control {
            Control::Toggle { .. } => Kind::Flag,
            Control::Number { min, max, .. } => Kind::Number { min, max },
            Control::Choice { options, .. } => Kind::Choice(options),
            Control::Gamepad => Kind::Gamepad,
        });
    }
    Some(match key {
        SYS_MODULES
        | "debug"
        | online::ONLINE
        | online::UPNP
        | online::BBSC
        | online::TLS
        | online::ALLOW_PLAINTEXT => Kind::Flag,
        online::BBSC_LOCAL_SERVER => Kind::LocalServer,
        frame_gen::KEY => Kind::FrameGen,
        local_player::KEY => Kind::LocalPlayer,
        "gamepad_name" => Kind::Name,
        "button_icons" => Kind::Word,
        "extra_env" => Kind::Env,
        _ if mods::FOLDER_KEYS.contains(&key) => Kind::Folder,
        _ => return None,
    })
}

/// Whether `key` is read from the launch settings rather than the port ini.
pub(super) fn is_launch_key(key: &str) -> bool {
    launch_kind(key).is_some() || LauncherSettings::is_shared(key)
}

/// The category of a launch key a title owns; `None` for one no title owns.
pub(super) fn launch_category(key: &str) -> Option<Category> {
    if !title::is_titles_own(key) {
        return None;
    }
    Some(match key {
        "gamepad" | "gamepad_name" | "button_icons" => Category::Gamepad,
        "mods_enabled" => Category::Mods,
        online::ONLINE
        | online::UPNP
        | online::BBSC
        | online::BBSC_LOCAL_SERVER
        | online::TLS
        | online::ALLOW_PLAINTEXT
        | local_player::KEY => Category::Online,
        "extra_env" | SYS_MODULES => Category::Extras,
        _ if mods::FOLDER_KEYS.contains(&key) => Category::Mods,
        _ => Category::Graphics,
    })
}

/// The category of a port ini key; `None` for the in-game menu's language, which follows the UI's.
pub(super) fn ini_category(key: &str) -> Option<Category> {
    if key == MENU_LANGUAGE {
        None
    } else if key.starts_with("key.") {
        Some(Category::Keyboard)
    } else if key.starts_with("pad.") {
        Some(Category::Gamepad)
    } else {
        Some(Category::Graphics)
    }
}

/// `value` for launch key `key`, checked. `library` is the library folder a mods or patches
/// folder must be inside.
pub(super) fn check_launch(key: &str, value: &Value, library: &Path) -> Result<(), Refusal> {
    if LauncherSettings::is_shared(key) {
        return Err(Refusal::SharedKey(key.to_owned()));
    }
    let kind = launch_kind(key)
        .filter(|_| title::is_titles_own(key))
        .ok_or_else(|| Refusal::UnknownKey(key.escape_default().to_string()))?;
    if matches!(kind, Kind::Folder) {
        return match value.as_str() {
            Some(folder) if folder.is_empty() || is_inside(folder, library) => Ok(()),
            Some(_) => Err(Refusal::Outside(key.to_owned())),
            None => Err(Refusal::Value(key.to_owned())),
        };
    }
    let text = value.as_str();
    let is_valid = match kind {
        Kind::Flag => value.is_boolean(),
        Kind::Number { min, max } => value
            .as_f64()
            .is_some_and(|number| number.is_finite() && (min..=max).contains(&number)),
        Kind::Choice(options) => text.is_some_and(|text| options.iter().any(|option| option.1 == text)),
        Kind::FrameGen => text.and_then(FrameGen::parse).is_some(),
        Kind::LocalServer => text.is_some_and(|text| LocalServer::ALL.iter().any(|mode| mode.value() == text)),
        Kind::LocalPlayer => text.is_some_and(|text| LocalPlayer::parse(text).is_ok()),
        Kind::Gamepad => text.is_some_and(|text| {
            text.len() <= MAX_GUID && text.bytes().all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        }),
        Kind::Name => text.is_some_and(|text| text.chars().count() <= MAX_NAME && !text.chars().any(char::is_control)),
        Kind::Word => text.is_some_and(is_word),
        Kind::Env => text.is_some_and(is_env),
        Kind::Folder => false,
    };
    if is_valid {
        Ok(())
    } else {
        Err(Refusal::Value(key.to_owned()))
    }
}

/// `text` from the command line as the JSON value `key` keeps: a switch takes `on`/`off`,
/// `true`/`false` or `1`/`0`; a number keeps whole numbers whole. Checked as [`check_launch`].
pub(super) fn launch_value(key: &str, text: &str, library: &Path) -> Result<Value, Refusal> {
    let value = match launch_kind(key) {
        Some(Kind::Flag) => match text {
            "on" | "true" | "1" => Value::Bool(true),
            "off" | "false" | "0" => Value::Bool(false),
            _ => return Err(Refusal::Value(key.to_owned())),
        },
        Some(Kind::Number { .. }) => match text.parse::<i64>() {
            Ok(whole) => Value::from(whole),
            Err(_) => text
                .parse::<f64>()
                .ok()
                .and_then(serde_json::Number::from_f64)
                .map(Value::Number)
                .ok_or_else(|| Refusal::Value(key.to_owned()))?,
        },
        _ => Value::String(text.to_owned()),
    };
    check_launch(key, &value, library).map(|()| value)
}

/// A port ini line decanter writes for a game: a key of the port's shape, a value on one line, and
/// for a key of the settings table one of the values it offers.
pub(super) fn check_ini(key: &str, value: &str) -> Result<(), Refusal> {
    check_ini_key(key)?;
    let is_line = value.len() <= MAX_INI_VALUE && !value.chars().any(char::is_control);
    let is_valid = is_line
        && rows()
            .find(|setting| setting.store == Store::Ini && setting.key == key)
            .is_none_or(|setting| match setting.control {
                Control::Toggle { .. } => matches!(value, "0" | "1"),
                Control::Number { min, max, .. } => value
                    .parse::<f64>()
                    .is_ok_and(|number| number.is_finite() && (min..=max).contains(&number)),
                Control::Choice { options, .. } => options.iter().any(|option| option.1 == value),
                Control::Gamepad => false,
            });
    if is_valid {
        Ok(())
    } else {
        Err(Refusal::Value(key.to_owned()))
    }
}

pub(super) fn check_ini_key(key: &str) -> Result<(), Refusal> {
    let is_key = (1..=MAX_INI_KEY).contains(&key.len())
        && key.bytes().next().is_some_and(|byte| byte.is_ascii_alphanumeric())
        && key
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'.');
    if is_key && ini_category(key).is_some() {
        Ok(())
    } else {
        Err(Refusal::UnknownKey(key.escape_default().to_string()))
    }
}

/// A mod's folder name or a patch's `<file>/<patch>` key: no way out of the folder it names.
pub(super) fn check_name(name: &str, has_patch: bool) -> Result<(), Refusal> {
    let mut parts = name.split('/');
    let is_valid = (1..=255).contains(&name.len())
        && !name.contains('\\')
        && !name.chars().any(char::is_control)
        && parts.next().is_some_and(is_plain_name)
        && (if has_patch {
            parts.next().is_some_and(|patch| !patch.is_empty())
        } else {
            parts.next().is_none()
        });
    if is_valid { Ok(()) } else { Err(Refusal::Name) }
}

fn is_plain_name(name: &str) -> bool {
    !name.is_empty() && name != "." && name != ".."
}

fn is_word(text: &str) -> bool {
    (1..=MAX_WORD).contains(&text.len())
        && text
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
}

/// What the launch accepts as `extra_env` (`NAME=value` items split on whitespace), never a shadNet
/// secret, which only the keyring holds.
fn is_env(text: &str) -> bool {
    text.len() <= MAX_ENV
        && !text.chars().any(|ch| ch.is_control() && !ch.is_whitespace())
        && text.split_whitespace().all(|item| {
            item.split_once('=').is_some_and(|(name, _)| {
                (1..=MAX_ENV_NAME).contains(&name.len())
                    && name.bytes().next().is_some_and(|byte| !byte.is_ascii_digit())
                    && name.bytes().all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
                    && !decanter_core::redact::is_secret_name(name)
            })
        })
}

/// `extra_env` without the shadNet secrets a user typed in it, for what leaves decanter's files:
/// the command line's output and an export.
pub(super) fn without_secrets(env: &str) -> String {
    env.split_whitespace()
        .filter(|item| {
            item.split_once('=')
                .is_none_or(|(name, _)| !decanter_core::redact::is_secret_name(name))
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// A full path inside `library`, with no `.` or `..` that could lead out of it.
fn is_inside(folder: &str, library: &Path) -> bool {
    let path = Path::new(folder);
    path.is_absolute()
        && path
            .components()
            .all(|part| matches!(part, Component::Prefix(_) | Component::RootDir | Component::Normal(_)))
        && path.starts_with(library)
}
