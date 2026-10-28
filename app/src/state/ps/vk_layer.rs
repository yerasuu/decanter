//! The Vulkan validation layer for a launch that asks the port to validate: realised on demand
//! (decision #474), or the launch goes on without validating and says why, since the port alone
//! would only log that the layer is missing.

use std::path::{Path, PathBuf};

use decanter_ps::bbport::Names;
use decanter_ps::os::linux::toolchain::Toolchain;
use decanter_ps::os::linux::toolchain::validation::{self, Unavailable};
use decanter_ps::paths::Root;
use decanter_ps::process::JobKind;
use gpui::{AsyncApp, WeakEntity};
use tokio::process::Command;

use super::Ps;
use super::jobs::{self, JobKey};
use crate::i18n::Text;

enum Outcome {
    NotAsked,
    /// What the run log's header says about the layer.
    Fact(String),
    Stopped,
}

/// Gives a launch `command` the layer when it asks the port to validate, or takes the request
/// back when no layer can be had, noting which in `facts`. A launch that already failed passes
/// through; stopping the layer's job fails it, so the game is not started either.
#[expect(
    clippy::too_many_arguments,
    reason = "it takes over the launch's command and facts on their way to the run"
)]
pub async fn apply(
    this: &WeakEntity<Ps>,
    root: &Root,
    toolchain: &Toolchain,
    game_dir: &Path,
    command: Result<Command, Text>,
    mut facts: Vec<(&'static str, String)>,
    cx: &mut AsyncApp,
) -> (Result<Command, Text>, Vec<(&'static str, String)>) {
    let mut command = match command {
        Ok(command) => command,
        Err(err) => return (Err(err), facts),
    };
    match give(this, root, toolchain, game_dir, &mut command, cx).await {
        Outcome::NotAsked => {}
        Outcome::Fact(fact) => facts.push(("vk_validation_layer", fact)),
        Outcome::Stopped => return (Err(Text::new("ps.vk_layer.stopped")), facts),
    }
    (Ok(command), facts)
}

async fn give(
    this: &WeakEntity<Ps>,
    root: &Root,
    toolchain: &Toolchain,
    game_dir: &Path,
    command: &mut Command,
    cx: &mut AsyncApp,
) -> Outcome {
    let validation_var = Names::of_command(command).var("VK_VALIDATION");
    let asked = command
        .as_std()
        .get_envs()
        .any(|(key, value)| key == validation_var.as_str() && value.is_some());
    if !asked {
        return Outcome::NotAsked;
    }
    match layer(this, root, toolchain, game_dir, cx).await {
        Ok(Some(dir)) => {
            if let Err(err) = validation::clear_unavailable(root).await {
                tracing::warn!(%err, "could not clear the validation layer's last failure");
            }
            tracing::info!(dir = %dir.display(), "Vulkan validation layer for the port");
            let fact = dir.display().to_string();
            command.env("VK_ADD_LAYER_PATH", dir);
            Outcome::Fact(fact)
        }
        Ok(None) => Outcome::Stopped,
        Err(why) => {
            tracing::warn!(%why, "Vulkan validation layer unavailable; the game starts without validating");
            command.env_remove(&validation_var);
            if let Err(err) = validation::record_unavailable(root, why).await {
                tracing::warn!(%err, "could not record why the validation layer is unavailable");
            }
            let _ = this.update(cx, |this, cx| {
                this.notice = Some((unavailable_text(why), true));
                cx.notify();
            });
            Outcome::Fact(format!("off, layer unavailable: {why}"))
        }
    }
}

/// `Ok(None)` when the user stopped its job.
async fn layer(
    this: &WeakEntity<Ps>,
    root: &Root,
    toolchain: &Toolchain,
    game_dir: &Path,
    cx: &mut AsyncApp,
) -> Result<Option<PathBuf>, Unavailable> {
    let realiser = match validation::realiser(toolchain).await {
        Ok(realiser) => realiser,
        Err(why) => return Err(why),
    };
    if let Some(dir) = validation::cached(root, &realiser).await {
        return Ok(Some(dir));
    }
    set_notice(this, Some(Text::new("ps.vk_layer.preparing")), cx);
    let command = match validation::realise_command(root, &realiser).await {
        Ok(command) => command,
        Err(err) => {
            tracing::warn!(%err, toolchain = realiser.id(), "could not set up the validation layer's realisation");
            return Err(Unavailable::Setup);
        }
    };
    let key = JobKey::Game(game_dir.to_owned());
    let facts = vec![
        ("toolchain", realiser.id().to_owned()),
        ("package", "vulkan-validation-layers".to_owned()),
    ];
    let finished = jobs::run(this, root, key.clone(), JobKind::VkLayer, facts, command, None, cx).await;
    if finished.succeeded() {
        set_notice(this, None, cx);
        return validation::record(root, &realiser).await.map(Some);
    }
    let stopped = this
        .update(cx, |this, cx| {
            this.jobs.get(&key).is_some_and(|job| job.read(cx).stop_requested)
        })
        .unwrap_or(false);
    if stopped { Ok(None) } else { Err(Unavailable::Build) }
}

fn set_notice(this: &WeakEntity<Ps>, text: Option<Text>, cx: &mut AsyncApp) {
    let _ = this.update(cx, |this, cx| {
        this.notice = text.map(|text| (text, false));
        cx.notify();
    });
}

/// For the launch's notice and the setting's note.
pub fn unavailable_text(why: Unavailable) -> Text {
    let reason = match why {
        Unavailable::NoNix => Text::new("ps.vk_layer.reasons.no_nix"),
        Unavailable::Setup => Text::new("ps.vk_layer.reasons.setup"),
        Unavailable::Build => Text::new("ps.vk_layer.reasons.build"),
    };
    Text::new("ps.vk_layer.unavailable").text("reason", reason)
}
