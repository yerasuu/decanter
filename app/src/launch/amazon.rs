//! What an Amazon Games title gets before it starts under Proton (stage S5 of
//! docs/amazon-nile-integration-analysis.md, sections 5.1 to 5.3): its `fuel.json`'s arguments and
//! folder, Amazon's SDK fetched into the library once, and the variables nile hands it. No
//! achievement hook and no cloud saves: Amazon offers decanter neither (section 6).

use std::path::Path;

use decanter_amazon::FuelLaunch;
use decanter_core::{InstalledGame, Library};
use decanter_runtime::Launch;
use rust_i18n::t;

/// Sets `launch` up for `game`; what failed, as lines for the run's log. Nothing here stops the
/// launch, since a game that never calls the SDK runs without any of it.
pub(super) async fn prepare(account_id: &str, game: &InstalledGame, launch: &mut Launch) -> Vec<String> {
    let product_id = game.game_id.as_str();
    let library = match crate::core::library_for_account(account_id) {
        Ok(library) => library,
        Err(err) => {
            tracing::warn!(account_id, product_id, %err, "no library, the amazon game starts without its sdk and fuel.json");
            return Vec::new();
        }
    };
    let fuel = fuel_note(game, &library, launch).await;
    let sdk = sdk_note(account_id, product_id, &library, launch).await;
    account_env(account_id, product_id, launch).await;
    fuel.into_iter().chain(sdk).collect()
}

async fn fuel_note(game: &InstalledGame, library: &Library, launch: &mut Launch) -> Option<String> {
    let install_dir = library.install_dir(&game.install_dir)?;
    let reason = match tokio::task::spawn_blocking(move || decanter_amazon::read_fuel(&install_dir)).await {
        Ok(Ok(fuel)) => {
            apply_fuel(game, fuel, launch);
            return None;
        }
        Ok(Err(err)) => format!("{err:#}"),
        Err(err) => err.to_string(),
    };
    tracing::warn!(game_id = %game.game_id, reason, "amazon game's fuel.json unusable, starting its program as it is");
    Some(t!("amazon.launch.fuel_failed", reason = reason).into_owned())
}

/// Amazon's SDK in the game's library folder, fetched on the first launch that finds it missing.
async fn sdk_note(account_id: &str, product_id: &str, library: &Library, launch: &mut Launch) -> Option<String> {
    let root = library
        .root_of(account_id, product_id)
        .unwrap_or_else(|| library.root().to_path_buf());
    match decanter_amazon::ensure_sdk(&root).await {
        Ok(sdk) => {
            // nile hands these over as host paths, which a Windows program resolves against its
            // current drive and so only finds while it starts on Z:; as Z: paths they hold from any
            // folder. UNVERIFIED with a game (analysis, section 5.1).
            set_path(launch, "FUEL_DIR", &sdk.fuel_dir);
            set_path(launch, "AMAZON_GAMES_SDK_PATH", &sdk.sdk_path);
            None
        }
        Err(err) => {
            let reason = format!("{err:#}");
            tracing::error!(
                account_id,
                product_id,
                reason,
                "amazon games sdk not available, the game starts without it"
            );
            Some(t!("amazon.launch.sdk_failed", reason = reason).into_owned())
        }
    }
}

/// Who the game is for: the ids the library listing cached and the account's name.
async fn account_env(account_id: &str, product_id: &str, launch: &mut Launch) {
    let cached = crate::views::library::amazon_install::cached_game(account_id, product_id);
    let ids = [
        (
            "AMAZON_GAMES_FUEL_ENTITLEMENT_ID",
            cached.as_ref().and_then(|cached| cached.entitlement_id.clone()),
        ),
        (
            "AMAZON_GAMES_FUEL_PRODUCT_SKU",
            cached.and_then(|cached| cached.product_sku),
        ),
    ];
    for (name, value) in ids {
        if let Some(value) = value {
            launch.env.push((name.to_owned(), value));
        } else {
            tracing::warn!(
                account_id,
                product_id,
                variable = name,
                "not cached by the library listing, left unset"
            );
        }
    }
    match decanter_amazon::given_name(account_id).await {
        Ok(Some(name)) => launch.env.push(("AMAZON_GAMES_FUEL_DISPLAY_NAME".to_owned(), name)),
        Ok(None) => tracing::info!(
            account_id,
            "amazon gave no given name, AMAZON_GAMES_FUEL_DISPLAY_NAME left unset"
        ),
        Err(err) => {
            tracing::warn!(account_id, error = %format_args!("{err:#}"), "amazon session unreadable, AMAZON_GAMES_FUEL_DISPLAY_NAME left unset");
        }
    }
}

/// The arguments and folder `fuel.json` gives its own program, `fuel.json`'s arguments first as nile
/// puts them before its user's. A program picked in the properties starts as it is, since they may
/// mean nothing to it.
fn apply_fuel(game: &InstalledGame, fuel: FuelLaunch, launch: &mut Launch) {
    if game.launch_executable.as_deref() != Some(fuel.command.as_str()) {
        tracing::info!(
            game_id = %game.game_id,
            program = ?game.launch_executable,
            fuel_command = fuel.command.as_str(),
            "the amazon game starts another program than its fuel.json's, without its arguments"
        );
        return;
    }
    // The game's folder is taken from the program's path rather than the install's, since mods may
    // run it from their play folder.
    let Some(root) = launch
        .executable
        .ancestors()
        .nth(fuel.command.as_path().components().count())
    else {
        return;
    };
    launch.working_directory = Some(
        fuel.working_subdir
            .as_ref()
            .map_or_else(|| root.to_path_buf(), |subdir| root.join(subdir.as_path())),
    );
    launch.args.splice(0..0, fuel.args);
}

/// `path` on the `Z:` drive every Wine prefix maps to the host's `/`.
fn set_path(launch: &mut Launch, name: &str, path: &Path) {
    if let Some(host) = path.to_str() {
        launch
            .env
            .push((name.to_owned(), format!("Z:{}", host.replace('/', "\\"))));
    } else {
        tracing::warn!(variable = name, path = %path.display(), "not a UTF-8 path, left unset");
    }
}
