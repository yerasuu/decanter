//! Launches a profile's installed GOG game and waits for it to exit.
//!
//! Resolved entirely from achv's own library (`achv_core::Library`): the
//! manifest carries the install directory and the launch executable, and
//! the Proton prefix lives wherever `Library::compatdata` says — created
//! on first launch, nothing to look up beforehand.
//!
//! Separate from the GUI's own `crate::launch` (which tracks a session for
//! the tray/exit-on-signal machinery) — this is the headless CLI path,
//! used for scripting and for observing a launch independent of the app.

use achv_core::{Library, Storefront};
use achv_runtime::Launch;
use anyhow::{Context, bail};

use super::report_poll;

pub async fn launch(db: &mut achv_core::Db, profile_id: &str, product_id: &str) -> anyhow::Result<()> {
    let Some(library_path) = Library::default_path() else {
        bail!("could not resolve the default library path");
    };
    let library = Library::new(library_path);

    let Some(game) = library.read_manifest(profile_id, Storefront::Gog, product_id) else {
        bail!("{product_id} isn't installed for this profile");
    };
    let Some(install_dir) = library.install_dir(&game.install_dir) else {
        bail!("unusable install directory for {product_id}");
    };
    let Some(executable) = game.launch_executable.as_deref() else {
        bail!("{product_id} is installed but has no launch executable set");
    };
    let Some(prefix_path) = library.compatdata(profile_id, product_id) else {
        bail!("unusable profile or product id for {product_id}'s prefix");
    };
    let prefix = achv_runtime::Prefix::new(prefix_path);

    // Capa 1 (hook DLL) placeholder: harmless with no achv-hook.dll
    // deployed yet, Wine just keeps using the real Galaxy64.dll. Wiring
    // the actual proxy is a separate crate (achv-hook).
    let mut launch = Launch::new(install_dir.join(executable), format!("umu-{product_id}")).store("gog");
    launch.dll_overrides.push(("Galaxy64".to_owned(), "n".to_owned()));

    let session = achv_runtime::start(&prefix, launch)
        .await
        .context("launching program")?;
    tracing::info!(pid = session.launcher_pid(), "launched");

    session.wait().await.context("running program")?;
    tracing::info!("program exited");

    report_poll(achv_gog::poll_achievements(db, profile_id, product_id).await?);
    Ok(())
}
