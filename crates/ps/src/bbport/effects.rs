//! A title's own effect switches, as the port lists them.
//!
//! `out/psport --list-effects` answers for any game it has them for. Ports without the query
//! (bbport, psport before it) keep the effects group of [`super::GROUPS`], which is Bloodborne's.

use std::collections::BTreeMap;

use serde::Deserialize;

use super::{Bbport, Names};
use crate::port::Port;
use crate::toolchain::Toolchain;

/// One of the overlay's sections of the title's effects, in the overlay's order.
#[derive(Clone, Deserialize)]
pub struct Section {
    /// Whether the `model_lod` choice comes first.
    pub model_detail: bool,
    pub hint: Option<Text>,
    pub effects: Vec<Effect>,
}

/// A switch of the title's own patches: a `psport.ini` key, `1` or `0`, read at start.
#[derive(Clone, Deserialize)]
pub struct Effect {
    pub key: String,
    /// The game's own behaviour, unpatched; what the port assumes while the key is absent.
    pub default: bool,
    pub name: Text,
    pub description: Option<Text>,
}

/// A text by language code, in the languages the port has it in.
#[derive(Clone, Deserialize)]
#[serde(transparent)]
pub struct Text(BTreeMap<String, String>);

impl Text {
    /// In `locale`'s language when the port has it, else in English.
    pub fn get(&self, locale: &str) -> &str {
        let language = locale.split(['-', '_']).next().unwrap_or(locale);
        self.0
            .get(language)
            .or_else(|| self.0.get("en"))
            .map_or("", String::as_str)
    }
}

pub fn parse(json: &[u8]) -> serde_json::Result<Vec<Section>> {
    serde_json::from_slice(json)
}

/// The title's sections, empty when the port has no effects for it.
///
/// `None` when the port cannot list them, so the caller falls back to Bloodborne's table. Runs the
/// port's binary, which answers before loading anything of the game.
pub async fn list(bbport: &Bbport, toolchain: &Toolchain, title_id: &str) -> Option<Vec<Section>> {
    let names = Names::of(&bbport.host_port_dir(toolchain).await).await;
    if !matches!(names, Names::Psport) {
        return None;
    }
    let script = format!(
        r#"exec "$DECANTER_PORT_DIR/{}" --list-effects --title "$DECANTER_TITLE""#,
        names.probe()
    );
    let mut command = match toolchain.command(&bbport.root, Port::Bbport, &script).await {
        Ok(command) => command,
        Err(err) => {
            tracing::warn!(%err, "port effects not queried");
            return None;
        }
    };
    command
        .env("DECANTER_PORT_DIR", bbport.port_dir().await)
        .env("DECANTER_TITLE", title_id);
    let output = match command.output().await {
        Ok(output) => output,
        Err(err) => {
            tracing::warn!(%err, "port effects not queried");
            return None;
        }
    };
    // A psport from before the query takes it for an unknown option and exits with 1.
    if !output.status.success() {
        tracing::debug!(
            title_id,
            status = %output.status,
            stderr = %String::from_utf8_lossy(&output.stderr).trim(),
            "port lists no effects, using the built-in table"
        );
        return None;
    }
    match parse(&output.stdout) {
        Ok(sections) => {
            tracing::debug!(
                title_id,
                effects = sections.iter().map(|section| section.effects.len()).sum::<usize>(),
                "port effects listed"
            );
            Some(sections)
        }
        Err(err) => {
            tracing::warn!(title_id, %err, "port effects unreadable");
            None
        }
    }
}
