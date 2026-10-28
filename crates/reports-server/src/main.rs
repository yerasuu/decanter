//! decanter-reports: receives decanter's error reports, re-validates them with the crate that built them
//! and publishes the ones that pass. Plain HTTP on loopback; TLS and the public port belong to the tunnel.

mod admin;
mod admin_logs;
#[cfg(test)]
mod admin_logs_tests;
#[cfg(test)]
mod admin_tests;
mod admin_ui;
mod app;
mod cache;
mod check;
mod clean_page;
mod config;
mod html;
mod ingest;
mod inspect;
mod limits;
mod logdiff;
mod logtext;
mod pages;
mod perms;
mod secrets;
mod server;
mod session;
mod store;
mod sweep;

use std::io::Write as _;
use std::path::PathBuf;
use std::process::ExitCode;

use tracing::level_filters::LevelFilter;

use crate::app::{AppState, router};
use crate::config::{Config, ConfigError, Driver, StorageCredentials};
use crate::session::AdminKeys;
use crate::store::{Product, Store, Stores};

enum Mode {
    Serve,
    Check,
    Inspect(inspect::Request),
    Secret,
}

struct Args {
    mode: Mode,
    config_path: Option<PathBuf>,
}

fn parse_args(args: &[String], env: &impl Fn(&str) -> Option<String>) -> Result<Args, String> {
    let env_path = || env("DECANTER_REPORTS_CONFIG").map(PathBuf::from);
    if let [only] = args
        && only == "secret"
    {
        return Ok(Args {
            mode: Mode::Secret,
            config_path: None,
        });
    }
    if let Some(("inspect", rest)) = args.split_first().map(|(first, rest)| (first.as_str(), rest)) {
        return Ok(Args {
            mode: Mode::Inspect(inspect::parse(rest)?),
            config_path: env_path(),
        });
    }
    let mut mode = Mode::Serve;
    let mut config_path = None;
    for arg in args {
        match arg.as_str() {
            "--check-config" => mode = Mode::Check,
            flag if flag.starts_with('-') => return Err(format!("unknown flag {flag}")),
            path if config_path.is_none() => config_path = Some(PathBuf::from(path)),
            _ => return Err("only one config path is accepted".to_owned()),
        }
    }
    Ok(Args {
        mode,
        config_path: config_path.or_else(env_path),
    })
}

async fn open_store(config: &Config, credentials: &StorageCredentials, product: Product) -> Result<Store, String> {
    let name = product.name();
    if config.storage.driver == Driver::Local {
        let dir = store::partition_dir(&config.storage, product);
        perms::private_dir(&dir).map_err(|error| format!("cannot prepare {}: {error}", dir.display()))?;
    }
    let backend = store::connect(&config.storage, credentials, product)
        .map_err(|error| format!("cannot set up the {name} partition: {error:#}"))?;
    Store::open(backend, &credentials.passphrase)
        .await
        .map_err(|error| format!("cannot open the {name} partition: {error:#}"))
}

/// Partitions that already have their encryption open first: a passphrase that does not open one of them stops the
/// start before a partition still without encryption gets a key of its own, which the other could never share.
async fn open_stores(config: &Config, credentials: &StorageCredentials) -> Result<Stores, String> {
    let mut order = Vec::with_capacity(Product::ALL.len());
    for product in Product::ALL {
        let initialised = store::is_initialised(&config.storage, credentials, product)
            .await
            .map_err(|error| format!("cannot reach the {} partition: {error:#}", product.name()))?;
        order.push((product, initialised));
    }
    order.sort_by_key(|(_, initialised)| !initialised);
    let (mut decanter, mut psport) = (None, None);
    for (index, &(product, _)) in order.iter().enumerate() {
        match open_store(config, credentials, product).await {
            Ok(opened) => match product {
                Product::Decanter => decanter = Some(opened),
                Product::Psport => psport = Some(opened),
            },
            Err(message) => {
                let blocked: Vec<&str> = order
                    .iter()
                    .skip(index + 1)
                    .filter(|(_, initialised)| !initialised)
                    .map(|(product, _)| product.name())
                    .collect();
                if blocked.is_empty() {
                    return Err(message);
                }
                return Err(format!(
                    "{message}; the {} partition's encryption was not created, so that both partitions keep one \
                     passphrase",
                    blocked.join(" and ")
                ));
            }
        }
    }
    decanter
        .zip(psport)
        .map(|(decanter, psport)| Stores::new(decanter, psport))
        .ok_or_else(|| "a partition was not opened".to_owned())
}

async fn open_state(config: Config, credentials: StorageCredentials) -> Result<AppState, String> {
    if config.storage.driver == Driver::Local {
        let dir = &config.storage.dir;
        perms::private_dir(dir).map_err(|error| format!("cannot prepare {}: {error}", dir.display()))?;
    }
    let stores = open_stores(&config, &credentials).await?;
    Ok(AppState::new(config, AdminKeys::default(), stores))
}

/// The secrets are already validated; this makes one read-only round trip per partition and prints no secret.
async fn check_config(config: &Config, credentials: &StorageCredentials) -> Result<(), String> {
    eprintln!("config ok: listening on {}", config.listen);
    if let Some(warning) = check::env_secrets_warning(config, check::in_container()) {
        eprintln!("warning: {warning}");
    }
    let (lines, passed) = check::partitions(config, credentials).await;
    for line in lines {
        eprintln!("{line}");
    }
    if passed {
        Ok(())
    } else {
        Err("a partition check failed".to_owned())
    }
}

async fn shutdown_signal() {
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut signal) => {
                signal.recv().await;
            }
            Err(_) => std::future::pending::<()>().await,
        }
    };
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {},
        () = terminate => {},
    }
}

async fn serve(state: AppState) -> Result<(), String> {
    let listen = state.config.listen;
    let listener = tokio::net::TcpListener::bind(listen)
        .await
        .map_err(|error| format!("cannot listen on {listen}: {error}"))?;
    tokio::spawn(sweep::daily(state.clone()));
    tokio::spawn(sweep::recover(state.clone()));
    tokio::spawn(session::warn_without_admins(state.clone()));
    tracing::info!(%listen, "listening");
    let limits = server::Limits::from_config(&state.config);
    server::run(listener, router(state), limits, shutdown_signal()).await;
    Ok(())
}

/// Only the secret reaches standard output, so `$(decanter-reports secret)` captures it whole.
fn print_secret() -> Result<(), String> {
    let secret = secrets::random_hex_256().map_err(|error| format!("the random source is unavailable: {error}"))?;
    std::io::stdout()
        .lock()
        .write_all(format!("{secret}\n").as_bytes())
        .map_err(|error| format!("cannot write the output: {error}"))
}

async fn start() -> Result<Option<(Config, StorageCredentials)>, String> {
    let env = |name: &str| std::env::var(name).ok();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let args = parse_args(&args, &env)?;
    if matches!(args.mode, Mode::Secret) {
        print_secret()?;
        return Ok(None);
    }
    let describe = |error: ConfigError| error.to_string();
    let config = Config::load(args.config_path.as_deref(), &env).map_err(describe)?;
    if let Mode::Inspect(request) = &args.mode {
        let credentials = config.storage_credentials(&env).map_err(describe)?;
        inspect::run(&config, &credentials, request).await?;
        return Ok(None);
    }
    let credentials = config.storage_credentials(&env).map_err(describe)?;
    for warning in config.warnings() {
        eprintln!("warning: {warning}");
    }
    if matches!(args.mode, Mode::Check) {
        check_config(&config, &credentials).await?;
        return Ok(None);
    }
    Ok(Some((config, credentials)))
}

#[tokio::main]
async fn main() -> ExitCode {
    perms::restrict_umask();
    let (config, credentials) = match start().await {
        Ok(Some(started)) => started,
        Ok(None) => return ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("decanter-reports: {message}");
            return ExitCode::FAILURE;
        }
    };
    let level = config.log_level.parse::<LevelFilter>().unwrap_or(LevelFilter::INFO);
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_max_level(level)
        .with_ansi(false)
        .init();
    let result = match open_state(config, credentials).await {
        Ok(state) => serve(state).await,
        Err(message) => Err(message),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            tracing::error!(%message, "stopped");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use std::error::Error;

    use super::*;

    fn credentials(passphrase: &str) -> StorageCredentials {
        StorageCredentials {
            passphrase: passphrase.to_owned(),
            bucket_keys: None,
        }
    }

    #[test]
    fn secret_takes_no_other_argument() {
        let no_env = |_: &str| None;
        let parse = |args: &[&str]| parse_args(&args.iter().map(|arg| (*arg).to_owned()).collect::<Vec<_>>(), &no_env);
        assert!(parse(&["secret"]).is_ok_and(|args| matches!(args.mode, Mode::Secret)));
        assert!(parse(&["secret", "extra"]).is_err());
        assert!(parse(&["config.toml"]).is_ok_and(|args| matches!(args.mode, Mode::Serve)));
    }

    #[tokio::test]
    async fn a_passphrase_that_does_not_open_one_partition_creates_no_other() -> Result<(), Box<dyn Error>> {
        let dir = std::env::temp_dir().join(format!("decanter-reports-guard-{}", secrets::random_hex()?));
        let mut config = Config::default();
        config.storage.dir.clone_from(&dir);
        perms::private_dir(&dir)?;
        let right = "the right passphrase, long enough for the guard";
        let wrong = "a wrong passphrase, just as long as the right one";
        drop(open_store(&config, &credentials(right), Product::Psport).await?);

        let message = open_stores(&config, &credentials(wrong))
            .await
            .err()
            .unwrap_or_default();
        assert!(message.contains("cannot open the psport partition"), "{message}");
        assert!(
            message.contains("the decanter partition's encryption was not created"),
            "{message}"
        );
        assert!(!message.contains(right) && !message.contains(wrong));
        assert!(!store::is_initialised(&config.storage, &credentials(right), Product::Decanter).await?);

        assert!(open_stores(&config, &credentials(right)).await.is_ok());
        assert!(store::is_initialised(&config.storage, &credentials(right), Product::Decanter).await?);
        std::fs::remove_dir_all(&dir)?;
        Ok(())
    }
}
