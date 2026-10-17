//! Launches an installed game through the runtime, for hook testing.
//! Proton only: on Windows the game runs natively and the hook isn't deployed.
#[cfg(not(windows))]
use core::time::Duration;
#[cfg(not(windows))]
use std::env;

#[cfg(not(windows))]
use decanter_core::{Db, Library, Storefront};
#[cfg(not(windows))]
use decanter_runtime::{Launch, Prefix, Proton};
#[cfg(not(windows))]
use tokio::time::sleep;

/// Split out from `main` so the `#[tokio::main]`-generated future stays a
/// plain single call instead of one large async stack frame.
#[expect(clippy::single_call_fn, reason = "split out to keep tokio::main's frame small")]
#[expect(
    clippy::print_stdout,
    reason = "this example's whole job is to narrate what it's doing"
)]
#[expect(
    clippy::implicit_return,
    reason = "needless_return elsewhere already wants no explicit return"
)]
#[expect(
    clippy::large_stack_frames,
    reason = "InstalledGame is inherently the largest local; not worth boxing in an example"
)]
#[cfg(not(windows))]
async fn run() -> anyhow::Result<()> {
    let game_id = env::args().nth(1).unwrap_or_else(|| "1854689188".into());
    let secs: u64 = env::args().nth(2).and_then(|value| value.parse().ok()).unwrap_or(60);

    let db = match Db::open_active() {
        Ok(db) => db,
        Err(err) => return Err(err.into()),
    };
    let profile = match db.active_profile() {
        Ok(Some(profile)) => profile.id,
        Ok(None) => return Err(anyhow::anyhow!("no active profile")),
        Err(err) => return Err(err.into()),
    };
    let Some(root) = decanter_core::bootstrap::active_root() else {
        return Err(anyhow::anyhow!("the active library isn't available"));
    };
    let library = Library::new(root);
    let Some(manifest) = library.read_manifest(&profile, Storefront::Gog, &game_id) else {
        return Err(anyhow::anyhow!("game {game_id} is not installed"));
    };
    let Some(dir) = library.install_dir(&manifest.install_dir) else {
        return Err(anyhow::anyhow!("install dir for {game_id} not found"));
    };
    let Some(executable) = manifest.launch_executable.as_deref() else {
        return Err(anyhow::anyhow!("{game_id} has no launch executable set"));
    };
    let exe = dir.join(executable);
    let Some(compatdata) = library.compatdata(&profile, &game_id) else {
        return Err(anyhow::anyhow!("no compatdata for {game_id}"));
    };
    let prefix = Prefix::new(compatdata);

    println!("exe    : {}", exe.display());
    println!("prefix : {}", prefix.path().display());

    let mut launch = Launch::new(exe, format!("umu-{game_id}")).store("gog");
    launch.proton = Proton::Latest;
    let session = match decanter_runtime::start(&prefix, launch).await {
        Ok(session) => session,
        Err(err) => return Err(err),
    };
    println!("lanzado, esperando {secs}s...");

    sleep(Duration::from_secs(secs)).await;
    println!("procesos vivos: {}", prefix.processes().len());
    if let Err(err) = prefix.stop().await {
        tracing::warn!(?err, "failed to stop prefix during shutdown");
    }
    drop(session);
    println!("parado.");
    Ok(())
}

#[cfg(not(windows))]
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    run().await
}

#[cfg(windows)]
fn main() {
    tracing::error!("launch_game drives Proton, which games don't run under on Windows");
}
