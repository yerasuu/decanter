//! What stands between each scanned dump and the play button.
//!
//! Building a port and converting a PS5 game are not in here: both happen
//! on their own when the game is played. What is left is what only the
//! user can change — a missing update, an encrypted executable.

use std::fs;
use std::path::PathBuf;

use crate::anyps5::{self, AnyPs5, Eboot};
use crate::bbport;
use crate::library::{Game, Platform};
use crate::port::Port;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    Ready,
    /// Ready, but the conversion runs first.
    NeedsConvert,
    /// A 1.09 update was found; merging it gives a playable copy.
    NeedsMerge(PathBuf),
    /// Another folder already holds this game at the supported version.
    HasPlayable(PathBuf),
    /// Bloodborne, but not the edition bbport runs.
    OtherEdition,
    NeedsUpdate,
    /// Another PS4 title: bbport starts it with the imports it lacks
    /// stubbed, and the log shows how far it gets.
    Experimental,
    Encrypted,
    NoEboot,
}

impl Status {
    pub const fn playable(&self) -> bool {
        matches!(self, Self::Ready | Self::NeedsConvert | Self::Experimental)
    }
}

pub struct Entry {
    pub game: Game,
    pub port: Option<Port>,
    pub status: Status,
}

/// Every playable dump with its status; bare updates are left out, since
/// they only matter as what a base game merges. Reads each PS5 executable's
/// header and conversion: off the UI thread.
pub fn classify(games: &[Game]) -> Vec<Entry> {
    games
        .iter()
        .filter(|game| !game.is_update())
        .map(|game| {
            let (port, status) = if bbport::is_bloodborne(game) {
                (Some(Port::Bbport), bloodborne_status(game, games))
            } else if game.platform == Platform::Ps5 {
                (Some(Port::AnyPs5), ps5_status(game))
            } else if game.dir.join("eboot.bin").is_file() {
                (Some(Port::Bbport), Status::Experimental)
            } else {
                (Some(Port::Bbport), Status::NoEboot)
            };
            Entry {
                game: game.clone(),
                port,
                status,
            }
        })
        .collect()
}

fn bloodborne_status(game: &Game, games: &[Game]) -> Status {
    if game.title_id != bbport::SUPPORTED_TITLE {
        return Status::OtherEdition;
    }
    if game.version == bbport::SUPPORTED_VERSION {
        return Status::Ready;
    }
    let playable = games.iter().find(|other| {
        !other.is_update() && other.title_id == game.title_id && other.version == bbport::SUPPORTED_VERSION
    });
    if let Some(playable) = playable {
        return Status::HasPlayable(playable.dir.clone());
    }
    match bbport::find_update(game, games) {
        Some(update) => Status::NeedsMerge(update.dir.clone()),
        None => Status::NeedsUpdate,
    }
}

fn ps5_status(game: &Game) -> Status {
    match anyps5::eboot_state(game) {
        Eboot::Missing => Status::NoEboot,
        Eboot::Encrypted => Status::Encrypted,
        Eboot::Clean => {
            let converted =
                fs::metadata(AnyPs5::output_dir(&game.title_id).join("app.elf")).is_ok_and(|meta| meta.is_file());
            if converted { Status::Ready } else { Status::NeedsConvert }
        }
    }
}
