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
mod logtext;
mod pages;
mod perms;
mod secrets;
mod server;
mod session;
mod store;
mod sweep;

use std::path::PathBuf;
use std::process::ExitCode;

use tracing::level_filters::LevelFilter;

use crate::app::{AppState, router};
use crate::config::{Config, ConfigError, Credentials, Driver, StorageCredentials};
use crate::store::{Product, Store, Stores};

enum Mode {
    Serve,
    Check,
    Inspect(inspect::Request),
}

struct Args {
    mode: Mode,
    config_path: Option<PathBuf>,
}

fn parse_args(args: &[String], env: &impl Fn(&str) -> Option<String>) -> Result<Args, String> {
    let env_path = || env("DECANTER_REPORTS_CONFIG").map(PathBuf::from);
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

async fn open_state(config: Config, credentials: Credentials) -> Result<AppState, String> {
    if config.storage.driver == Driver::Local {
        let dir = &config.storage.dir;
        perms::private_dir(dir).map_err(|error| format!("cannot prepare {}: {error}", dir.display()))?;
    }
    let decanter = open_store(&config, &credentials.storage, Product::Decanter).await?;
    let psport = open_store(&config, &credentials.storage, Product::Psport).await?;
    Ok(AppState::new(
        config,
        &credentials.admin_key,
        Stores::new(decanter, psport),
    ))
}

/// The secrets are already validated; this makes one read-only round trip per partition and prints no secret.
async fn check_config(config: &Config, credentials: &Credentials) -> Result<(), String> {
    eprintln!("config ok: listening on {}", config.listen);
    let (lines, passed) = check::partitions(config, &credentials.storage).await;
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
    tracing::info!(%listen, "listening");
    let limits = server::Limits::from_config(&state.config);
    server::run(listener, router(state), limits, shutdown_signal()).await;
    Ok(())
}

async fn start() -> Result<Option<(Config, Credentials)>, String> {
    let env = |name: &str| std::env::var(name).ok();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let args = parse_args(&args, &env)?;
    let describe = |error: ConfigError| error.to_string();
    let config = Config::load(args.config_path.as_deref(), &env).map_err(describe)?;
    if let Mode::Inspect(request) = &args.mode {
        let credentials = config.storage_credentials(&env).map_err(describe)?;
        inspect::run(&config, &credentials, request).await?;
        return Ok(None);
    }
    let credentials = config.credentials(&env).map_err(describe)?;
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
