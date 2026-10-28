//! decanter-reports: receives decanter's error reports, re-validates them with the crate that built them
//! and publishes the ones that pass. Plain HTTP on loopback; TLS and the public port belong to the tunnel.

mod admin;
mod app;
mod cache;
mod config;
mod html;
mod ingest;
mod limits;
mod pages;
mod perms;
mod secrets;
mod server;
mod store;
mod sweep;

use std::path::PathBuf;
use std::process::ExitCode;

use tracing::level_filters::LevelFilter;

use crate::app::{AppState, router};
use crate::config::{Config, ConfigError, Credentials, Driver};
use crate::store::Store;

struct Args {
    check_only: bool,
    config_path: Option<PathBuf>,
}

fn parse_args(args: impl Iterator<Item = String>, env: &impl Fn(&str) -> Option<String>) -> Result<Args, String> {
    let mut check_only = false;
    let mut config_path = None;
    for arg in args {
        match arg.as_str() {
            "--check-config" => check_only = true,
            flag if flag.starts_with('-') => return Err(format!("unknown flag {flag}")),
            path if config_path.is_none() => config_path = Some(PathBuf::from(path)),
            _ => return Err("only one config path is accepted".to_owned()),
        }
    }
    let config_path = config_path.or_else(|| env("DECANTER_REPORTS_CONFIG").map(PathBuf::from));
    Ok(Args {
        check_only,
        config_path,
    })
}

async fn open_state(config: Config, credentials: Credentials) -> Result<AppState, String> {
    if config.storage.driver == Driver::Local {
        let dir = &config.storage.dir;
        perms::private_dir(dir).map_err(|error| format!("cannot prepare {}: {error}", dir.display()))?;
    }
    let backend = store::connect(&config.storage, &credentials)
        .map_err(|error| format!("cannot set up the storage: {error:#}"))?;
    let store = Store::open(backend, &credentials.passphrase)
        .await
        .map_err(|error| format!("cannot open the storage: {error:#}"))?;
    Ok(AppState::new(config, &credentials.admin_key, store))
}

fn describe_storage(config: &Config) -> String {
    let storage = &config.storage;
    match storage.driver {
        Driver::Local => format!("local directory {}", storage.dir.display()),
        Driver::S3 | Driver::R2 => format!(
            "bucket {} at {} (prefix {:?})",
            storage.bucket,
            if storage.endpoint.is_empty() {
                "the provider"
            } else {
                storage.endpoint.as_str()
            },
            storage.prefix
        ),
    }
}

/// Validates the secrets and, for a bucket, makes one read-only round trip; prints no secret.
async fn check(config: &Config, credentials: &Credentials) -> Result<(), String> {
    eprintln!(
        "config ok: listening on {}, storage in {}",
        config.listen,
        describe_storage(config)
    );
    if config.storage.driver == Driver::Local {
        return Ok(());
    }
    let backend = store::connect(&config.storage, credentials)
        .map_err(|error| format!("cannot set up the storage: {error:#}"))?;
    match store::probe(backend.as_ref(), &credentials.passphrase).await {
        Ok(true) => eprintln!("bucket reachable and the passphrase opens it"),
        Ok(false) => eprintln!("bucket reachable, not initialized yet (the first start creates its encryption)"),
        Err(error) => return Err(format!("bucket check failed: {error:#}")),
    }
    Ok(())
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
    let args = parse_args(std::env::args().skip(1), &env)?;
    let describe = |error: ConfigError| error.to_string();
    let config = Config::load(args.config_path.as_deref(), &env).map_err(describe)?;
    let credentials = config.credentials(&env).map_err(describe)?;
    for warning in config.warnings() {
        eprintln!("warning: {warning}");
    }
    if args.check_only {
        check(&config, &credentials).await?;
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
