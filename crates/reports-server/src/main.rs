//! decanter-reports: receives decanter's error reports, re-validates them with the crate that built them
//! and publishes the ones that pass. Plain HTTP on loopback; TLS and the public port belong to the tunnel.

mod accounts;
mod admin;
mod admin_logs;
#[cfg(test)]
mod admin_logs_tests;
#[cfg(test)]
mod admin_tests;
mod admin_ui;
mod app;
mod audit;
#[cfg(test)]
mod audit_tests;
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

use crate::accounts::Accounts;
use crate::app::{AppState, router};
use crate::config::{Config, ConfigError, Driver, StorageCredentials};
use crate::store::{AccountStore, Product, Section, Store, Stores};

enum Mode {
    Serve,
    Check,
    Inspect(inspect::Request),
    User(accounts::cli::Command),
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
    match args.split_first().map(|(first, rest)| (first.as_str(), rest)) {
        Some(("inspect", rest)) => {
            return Ok(Args {
                mode: Mode::Inspect(inspect::parse(rest)?),
                config_path: env_path(),
            });
        }
        Some(("user", rest)) => {
            return Ok(Args {
                mode: Mode::User(accounts::cli::parse(rest)?),
                config_path: env_path(),
            });
        }
        _ => {}
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

/// The local driver's redb file takes a lock for as long as a process holds it open, so a second process (a `user`
/// command next to a running server) fails with this; the message says what to do instead of only that.
fn explain_lock(config: &Config, message: String) -> String {
    if config.storage.driver == Driver::Local && message.contains("Database already open") {
        format!("{message} (with the local driver the database is locked while the server runs: stop it first)")
    } else {
        message
    }
}

enum Opened {
    Reports(Product, Store),
    Accounts(AccountStore),
}

async fn open_section(config: &Config, credentials: &StorageCredentials, section: Section) -> Result<Opened, String> {
    let name = section.name();
    if config.storage.driver == Driver::Local {
        let dir = store::partition_dir(&config.storage, section);
        perms::private_dir(&dir).map_err(|error| format!("cannot prepare {}: {error}", dir.display()))?;
    }
    let backend = store::connect(&config.storage, credentials, section)
        .map_err(|error| format!("cannot set up the {name} store: {error:#}"))?;
    let opened = match section {
        Section::Reports(product) => Store::open(backend, &credentials.passphrase)
            .await
            .map(|store| Opened::Reports(product, store)),
        Section::Accounts => AccountStore::open(backend, &credentials.passphrase)
            .await
            .map(Opened::Accounts),
    };
    opened.map_err(|error| explain_lock(config, format!("cannot open the {name} store: {error:#}")))
}

/// Read-only: whether the passphrase opens an initialised section, without recovering or creating anything.
async fn probe_section(config: &Config, credentials: &StorageCredentials, section: Section) -> Result<(), String> {
    let name = section.name();
    let backend = store::connect(&config.storage, credentials, section)
        .map_err(|error| format!("cannot set up the {name} store: {error:#}"))?;
    store::probe(backend.as_ref(), &credentials.passphrase)
        .await
        .map(drop)
        .map_err(|error| explain_lock(config, format!("cannot open the {name} store: {error:#}")))
}

/// Opens `wanted`, looking at every initialised section first: a passphrase that does not open one of them stops
/// before a section still without encryption gets a key of its own, which the others could never share. Sections
/// not wanted are only probed.
async fn open_sections(
    config: &Config,
    credentials: &StorageCredentials,
    wanted: &[Section],
) -> Result<Vec<Opened>, String> {
    let mut order = Vec::with_capacity(Section::ALL.len());
    for section in Section::ALL {
        let initialised = store::is_initialised(&config.storage, credentials, section)
            .await
            .map_err(|error| explain_lock(config, format!("cannot reach the {} store: {error:#}", section.name())))?;
        order.push((section, initialised));
    }
    order.sort_by_key(|(_, initialised)| !initialised);
    let mut opened = Vec::with_capacity(wanted.len());
    for (index, &(section, initialised)) in order.iter().enumerate() {
        let step = if wanted.contains(&section) {
            open_section(config, credentials, section)
                .await
                .map(|one| opened.push(one))
        } else if initialised {
            probe_section(config, credentials, section).await
        } else {
            Ok(())
        };
        if let Err(message) = step {
            let blocked: Vec<&str> = order
                .iter()
                .skip(index + 1)
                .filter(|(section, initialised)| !initialised && wanted.contains(section))
                .map(|(section, _)| section.name())
                .collect();
            if blocked.is_empty() {
                return Err(message);
            }
            return Err(format!(
                "{message}; the encryption of {} was not created, so that every store keeps one passphrase",
                blocked.join(" and ")
            ));
        }
    }
    Ok(opened)
}

async fn open_state(config: Config, credentials: StorageCredentials) -> Result<AppState, String> {
    if config.storage.driver == Driver::Local {
        let dir = &config.storage.dir;
        perms::private_dir(dir).map_err(|error| format!("cannot prepare {}: {error}", dir.display()))?;
    }
    let (mut decanter, mut psport, mut account_store) = (None, None, None);
    for opened in open_sections(&config, &credentials, &Section::ALL).await? {
        match opened {
            Opened::Reports(Product::Decanter, store) => decanter = Some(store),
            Opened::Reports(Product::Psport, store) => psport = Some(store),
            Opened::Accounts(store) => account_store = Some(store),
        }
    }
    let (Some(decanter), Some(psport), Some(account_store)) = (decanter, psport, account_store) else {
        return Err("a store was not opened".to_owned());
    };
    let accounts = Accounts::new(account_store);
    accounts
        .refresh()
        .await
        .map_err(|error| format!("cannot read the accounts: {error:#}"))?;
    Ok(AppState::new(config, accounts, Stores::new(decanter, psport)))
}

/// The accounts store alone, for `user ...`, after the same passphrase guard as the server's start.
async fn open_accounts(config: &Config, credentials: &StorageCredentials) -> Result<AccountStore, String> {
    if config.storage.driver == Driver::Local {
        let dir = &config.storage.dir;
        perms::private_dir(dir).map_err(|error| format!("cannot prepare {}: {error}", dir.display()))?;
    }
    match open_sections(config, credentials, &[Section::Accounts]).await?.pop() {
        Some(Opened::Accounts(store)) => Ok(store),
        _ => Err("the accounts store was not opened".to_owned()),
    }
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
    tokio::spawn(accounts::refresh_loop(state.clone()));
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
    if let Mode::User(command) = &args.mode {
        let credentials = config.storage_credentials(&env).map_err(describe)?;
        let store = open_accounts(&config, &credentials).await?;
        accounts::cli::run(&store, command, &mut std::io::stdout(), &mut std::io::stderr()).await?;
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

    #[test]
    fn user_commands_take_their_arguments_and_the_config_from_the_environment() {
        let env = |name: &str| (name == "DECANTER_REPORTS_CONFIG").then(|| "/etc/reports.toml".to_owned());
        let parse = |args: &[&str]| parse_args(&args.iter().map(|arg| (*arg).to_owned()).collect::<Vec<_>>(), &env);
        let add = parse(&["user", "add", "owner", "--role", "admin"]);
        assert!(add.is_ok_and(|args| matches!(args.mode, Mode::User(_))
            && args.config_path.as_deref() == Some(std::path::Path::new("/etc/reports.toml"))));
        assert!(parse(&["user", "add", "owner"]).is_err());
        assert!(parse(&["user"]).is_err());
    }

    fn temp_config(name: &str) -> Result<Config, Box<dyn Error>> {
        let dir = std::env::temp_dir().join(format!("decanter-reports-{name}-{}", secrets::random_hex()?));
        let mut config = Config::default();
        config.storage.dir.clone_from(&dir);
        perms::private_dir(&dir)?;
        Ok(config)
    }

    #[tokio::test]
    async fn a_passphrase_that_does_not_open_one_partition_creates_no_other() -> Result<(), Box<dyn Error>> {
        let config = temp_config("guard")?;
        let right = "the right passphrase, long enough for the guard";
        let wrong = "a wrong passphrase, just as long as the right one";
        let psport = Section::Reports(Product::Psport);
        drop(open_section(&config, &credentials(right), psport).await?);

        let message = open_state(config.clone(), credentials(wrong))
            .await
            .err()
            .unwrap_or_default();
        assert!(message.contains("cannot open the psport store"), "{message}");
        assert!(
            message.contains("the encryption of decanter and accounts was not created"),
            "{message}"
        );
        assert!(!message.contains(right) && !message.contains(wrong));
        for section in [Section::Reports(Product::Decanter), Section::Accounts] {
            assert!(!store::is_initialised(&config.storage, &credentials(right), section).await?);
        }

        let message = open_accounts(&config, &credentials(wrong))
            .await
            .err()
            .unwrap_or_default();
        assert!(
            message.contains("the encryption of accounts was not created"),
            "{message}"
        );
        assert!(!store::is_initialised(&config.storage, &credentials(right), Section::Accounts).await?);

        assert!(open_state(config.clone(), credentials(right)).await.is_ok());
        assert!(store::is_initialised(&config.storage, &credentials(right), Section::Accounts).await?);
        std::fs::remove_dir_all(&config.storage.dir)?;
        Ok(())
    }

    #[tokio::test]
    async fn user_add_prints_a_key_the_router_accepts() -> Result<(), Box<dyn Error>> {
        use axum::body::Body;
        use axum::http::{Request, StatusCode};
        use tower::ServiceExt;

        let config = temp_config("cli")?;
        let text = "a passphrase for the end to end account test";
        let passphrase = credentials(text);
        let mut out = Vec::new();
        let mut notes = Vec::new();
        {
            let store = open_accounts(&config, &passphrase).await?;
            let command = accounts::cli::parse(&["add".into(), "owner".into(), "--role".into(), "admin".into()])?;
            accounts::cli::run(&store, &command, &mut out, &mut notes).await?;
        }
        let key = String::from_utf8(out)?.trim_end().to_owned();
        assert!(key.starts_with("drk_"));
        assert!(!String::from_utf8(notes)?.contains(&key));

        let state = open_state(config.clone(), credentials(text)).await?;
        assert!(state.accounts.has_admin());
        let locked = open_accounts(&config, &passphrase).await.err().unwrap_or_default();
        assert!(locked.contains("stop it first"), "{locked}");

        let mut request = Request::builder()
            .uri("/admin/reports")
            .header("authorization", format!("Bearer {key}"))
            .body(Body::empty())?;
        request
            .extensions_mut()
            .insert(axum::extract::ConnectInfo(std::net::SocketAddr::from((
                [203, 0, 113, 7],
                4000,
            ))));
        let response = router(state.clone()).oneshot(request).await?;
        assert_eq!(response.status(), StatusCode::OK);
        drop(state);
        std::fs::remove_dir_all(&config.storage.dir)?;
        Ok(())
    }
}
