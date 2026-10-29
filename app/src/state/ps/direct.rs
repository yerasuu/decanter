//! Playing on Windows: the `psport.exe` of the folder picked in the section's Ajustes, else of the
//! Windows package in use ([`super::package`]), started directly in a session of its own (a Job
//! Object), with no toolchain, bash or `run.sh`.

use std::path::Path;

use decanter_ps::library::Game;
use decanter_ps::paths::Root;
use decanter_ps::port::Port;
use gpui::{AsyncApp, Context, WeakEntity};

use super::Ps;
use super::jobs::{JobKey, Launcher};
use crate::i18n::Text;
use crate::ps_play::direct;
use crate::ps_play::launch::launch_inputs;

/// What a direct launch is for: the game, whose profile plays it, and where its caches go.
pub(super) struct Request<'launch> {
    pub(super) root: &'launch Root,
    pub(super) port: Port,
    pub(super) game: &'launch Game,
    pub(super) profile_id: &'launch str,
    pub(super) cache_dir: Option<&'launch Path>,
}

/// `psport.exe` with the game's environment and `run.sh`'s arguments, ready to spawn, and the
/// facts for its log's header. `this` hears of a first download of the package.
pub(super) async fn launch(
    this: &WeakEntity<Ps>,
    request: Request<'_>,
    cx: &mut AsyncApp,
) -> Result<(Launcher, Vec<(&'static str, String)>), Text> {
    let Request {
        root,
        port,
        game,
        profile_id,
        cache_dir,
    } = request;
    if let Err(err) = direct::check_port(port, game) {
        return Err(err);
    }
    let surroundings = super::play::surroundings(cx);
    let inputs = match launch_inputs(root, None, game, profile_id, surroundings).await {
        Ok(inputs) => inputs,
        Err(err) => {
            tracing::warn!(title_id = %game.title_id, error = %err, "could not start game");
            return Err(err.localized());
        }
    };
    let dir = match direct::chosen_dir(root, &inputs.settings).await {
        Some(dir) => dir,
        None => match super::package::for_play(this, root, cx).await {
            Ok(dir) => dir,
            Err(err) => return Err(err),
        },
    };
    match direct::session(root, game, &inputs, cache_dir, dir, &[]).await {
        Ok((prepared, facts)) => Ok((Launcher::Session(prepared), facts)),
        Err(err) => Err(err),
    }
}

/// How [`stop`] ends a game's session.
#[derive(Clone, Copy)]
pub(super) enum Ending {
    /// Asked to close first, so the game can save, and ended if it won't.
    Polite,
    /// Asked only through its stop file, which `psport.exe` ends in order on, and ended if it won't.
    Now,
}

/// Stops the game's session unless a stop of it is already under way; the exit it causes reads as a
/// stop.
pub(super) fn stop(ps: &mut Ps, dir: &Path, ending: Ending, cx: &mut Context<Ps>) {
    let Some(job) = ps.jobs.get(&JobKey::Game(dir.to_owned())) else {
        return;
    };
    if !job.read(cx).is_running() || !ps.stopping.insert(dir.to_owned()) {
        return;
    }
    job.update(cx, |job, _| job.stop_requested = true);
    cx.notify();
    let key = direct::session_key(dir);
    let dir = dir.to_owned();
    cx.spawn(async move |this, cx| {
        match ending {
            Ending::Polite => decanter_runtime::stop(&key).await,
            Ending::Now => decanter_runtime::end(&key).await,
        }
        let _ = this.update(cx, |this, cx| {
            this.stopping.remove(&dir);
            cx.notify();
        });
    })
    .detach();
}
