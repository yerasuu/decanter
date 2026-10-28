//! Settings from a TOML file, with environment overrides for what differs per host or is secret.

use std::fmt;
use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};

use axum::http::HeaderName;
use serde::Deserialize;
use tracing::level_filters::LevelFilter;

pub const DEFAULT_ADMIN_KEY_ENV: &str = "DECANTER_REPORTS_ADMIN_KEY";
pub const DEFAULT_PASSPHRASE_ENV: &str = "DECANTER_REPORTS_PASSPHRASE";
pub const DEFAULT_ACCESS_KEY_ID_ENV: &str = "DECANTER_REPORTS_S3_ACCESS_KEY_ID";
pub const DEFAULT_SECRET_ACCESS_KEY_ENV: &str = "DECANTER_REPORTS_S3_SECRET_ACCESS_KEY";
const CREDENTIALS_DIR_PREFIX: &str = "$CREDENTIALS_DIRECTORY";
const MIN_ADMIN_KEY_LEN: usize = 32;
/// The passphrase derives the key every object in the bucket is encrypted with; a short one is guessable offline by
/// anyone who can read the bucket.
const MIN_PASSPHRASE_LEN: usize = 32;

#[derive(Debug)]
pub struct ConfigError(String);

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ConfigError {}

fn err<T>(message: impl Into<String>) -> Result<T, ConfigError> {
    Err(ConfigError(message.into()))
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Limits {
    pub per_ip_hour: u32,
    pub per_ip_day: u32,
    pub global_hour: u32,
    pub get_per_ip_minute: u32,
    /// Admin authentication failures and wrong delete codes one address may make per `failure_window_secs`.
    pub failures_per_ip: u32,
    pub failure_window_secs: u32,
    pub max_reports: u32,
    pub max_attachments: u32,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            per_ip_hour: 10,
            per_ip_day: 50,
            global_hour: 500,
            get_per_ip_minute: 120,
            failures_per_ip: 10,
            failure_window_secs: 600,
            max_reports: 100_000,
            max_attachments: 2_000,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Driver {
    /// A directory on this host (one redb file): development, tests and single-host setups.
    #[default]
    Local,
    S3,
    R2,
}

impl std::str::FromStr for Driver {
    type Err = ConfigError;

    fn from_str(text: &str) -> Result<Self, ConfigError> {
        match text.trim() {
            "local" => Ok(Self::Local),
            "s3" => Ok(Self::S3),
            "r2" => Ok(Self::R2),
            _ => err("storage.driver must be local, s3 or r2"),
        }
    }
}

/// Where the reports live. Credentials are never here: only the names of the variables or files holding them.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Storage {
    pub driver: Driver,
    /// The `local` driver's directory; created 0700.
    pub dir: PathBuf,
    pub bucket: String,
    /// The S3 endpoint (a `MinIO` URL); empty means AWS itself.
    pub endpoint: String,
    pub region: String,
    /// The Cloudflare account of an `r2` bucket.
    pub account_id: String,
    /// Key prefix inside the bucket, so its user can be limited to it; `s3` and `r2` only.
    pub prefix: String,
    pub path_style: bool,
    /// Lets an `http://` endpoint through; credentials and objects then cross the network in the clear.
    pub allow_insecure_http: bool,
    pub access_key_id_env: String,
    pub access_key_id_file: Option<PathBuf>,
    pub secret_access_key_env: String,
    pub secret_access_key_file: Option<PathBuf>,
    pub passphrase_env: String,
    pub passphrase_file: Option<PathBuf>,
}

impl Default for Storage {
    fn default() -> Self {
        Self {
            driver: Driver::Local,
            dir: PathBuf::from("data"),
            bucket: String::new(),
            endpoint: String::new(),
            region: String::new(),
            account_id: String::new(),
            prefix: String::new(),
            path_style: false,
            allow_insecure_http: false,
            access_key_id_env: DEFAULT_ACCESS_KEY_ID_ENV.to_owned(),
            access_key_id_file: None,
            secret_access_key_env: DEFAULT_SECRET_ACCESS_KEY_ENV.to_owned(),
            secret_access_key_file: None,
            passphrase_env: DEFAULT_PASSPHRASE_ENV.to_owned(),
            passphrase_file: None,
        }
    }
}

/// The secrets the server runs with, read once at startup. No `Debug`, so none can reach a log by accident.
pub struct Credentials {
    pub admin_key: String,
    pub passphrase: String,
    /// Access key id and secret access key, for the `s3` and `r2` drivers.
    pub bucket_keys: Option<(String, String)>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Config {
    pub listen: SocketAddr,
    /// Prefix of the `url` the upload answers with; empty gives a path only.
    pub public_url: String,
    /// Environment variable holding the admin key; ignored when `admin_key_file` is set.
    pub admin_key_env: String,
    /// A path readable only by its owner; a leading `$CREDENTIALS_DIRECTORY` is replaced by systemd's credential
    /// directory (`LoadCredential=`), whose files are exempt from the mode check.
    pub admin_key_file: Option<PathBuf>,
    /// Only requests whose socket address is listed here may name another client in `client_ip_header`.
    pub trusted_proxies: Vec<IpAddr>,
    pub client_ip_header: Option<String>,
    pub retention_days: u32,
    /// Days a resolved report stays public after its resolution; 0 keeps them for good.
    pub resolved_retention_days: u32,
    pub page_size: u32,
    /// Seconds a rendered index page is reused; 0 turns the cache off.
    pub index_cache_secs: u32,
    pub log_level: String,
    /// Simultaneous connections; one more is closed at once.
    pub max_connections: u32,
    /// Seconds to receive a request head: bounds slow headers and idle keep-alive connections alike.
    pub header_timeout_secs: u32,
    /// Seconds open connections get to finish after SIGTERM or Ctrl-C before they are dropped.
    pub shutdown_grace_secs: u32,
    pub limits: Limits,
    pub storage: Storage,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            listen: SocketAddr::from(([127, 0, 0, 1], 8787)),
            public_url: String::new(),
            admin_key_env: DEFAULT_ADMIN_KEY_ENV.to_owned(),
            admin_key_file: None,
            trusted_proxies: Vec::new(),
            client_ip_header: None,
            retention_days: 365,
            resolved_retention_days: 90,
            page_size: 25,
            index_cache_secs: 15,
            log_level: "info".to_owned(),
            max_connections: 512,
            header_timeout_secs: 10,
            shutdown_grace_secs: 5,
            limits: Limits::default(),
            storage: Storage::default(),
        }
    }
}

impl Config {
    /// Reads the file (if any), applies the environment on top and validates the result.
    ///
    /// # Errors
    ///
    /// The file is unreadable or malformed, an override does not parse, or a value is out of range.
    pub fn load(path: Option<&Path>, env: &impl Fn(&str) -> Option<String>) -> Result<Self, ConfigError> {
        let mut config = match path {
            Some(path) => {
                let text = std::fs::read_to_string(path)
                    .map_err(|error| ConfigError(format!("cannot read {}: {error}", path.display())))?;
                toml::from_str(&text).map_err(|error| ConfigError(format!("{}: {error}", path.display())))?
            }
            None => Self::default(),
        };
        config.apply_env(env)?;
        config.validate()?;
        Ok(config)
    }

    fn apply_env(&mut self, env: &impl Fn(&str) -> Option<String>) -> Result<(), ConfigError> {
        fn parse<T: std::str::FromStr>(name: &str, value: &str) -> Result<T, ConfigError> {
            value
                .trim()
                .parse()
                .map_err(|_| ConfigError(format!("{name} does not parse")))
        }
        if let Some(v) = env("DECANTER_REPORTS_LISTEN") {
            self.listen = parse("DECANTER_REPORTS_LISTEN", &v)?;
        }
        if let Some(v) = env("DECANTER_REPORTS_PUBLIC_URL") {
            self.public_url = v;
        }
        if let Some(v) = env("DECANTER_REPORTS_ADMIN_KEY_ENV") {
            self.admin_key_env = v;
        }
        if let Some(v) = env("DECANTER_REPORTS_ADMIN_KEY_FILE") {
            self.admin_key_file = Some(PathBuf::from(v));
        }
        if let Some(v) = env("DECANTER_REPORTS_TRUSTED_PROXIES") {
            self.trusted_proxies = v
                .split(',')
                .map(str::trim)
                .filter(|part| !part.is_empty())
                .map(|part| parse("DECANTER_REPORTS_TRUSTED_PROXIES", part))
                .collect::<Result<_, _>>()?;
        }
        if let Some(v) = env("DECANTER_REPORTS_CLIENT_IP_HEADER") {
            self.client_ip_header = Some(v);
        }
        if let Some(v) = env("DECANTER_REPORTS_RETENTION_DAYS") {
            self.retention_days = parse("DECANTER_REPORTS_RETENTION_DAYS", &v)?;
        }
        if let Some(v) = env("DECANTER_REPORTS_RESOLVED_RETENTION_DAYS") {
            self.resolved_retention_days = parse("DECANTER_REPORTS_RESOLVED_RETENTION_DAYS", &v)?;
        }
        if let Some(v) = env("DECANTER_REPORTS_INDEX_CACHE_SECS") {
            self.index_cache_secs = parse("DECANTER_REPORTS_INDEX_CACHE_SECS", &v)?;
        }
        if let Some(v) = env("DECANTER_REPORTS_LOG") {
            self.log_level = v;
        }
        if let Some(v) = env("DECANTER_REPORTS_MAX_CONNECTIONS") {
            self.max_connections = parse("DECANTER_REPORTS_MAX_CONNECTIONS", &v)?;
        }
        if let Some(v) = env("DECANTER_REPORTS_HEADER_TIMEOUT_SECS") {
            self.header_timeout_secs = parse("DECANTER_REPORTS_HEADER_TIMEOUT_SECS", &v)?;
        }
        if let Some(v) = env("DECANTER_REPORTS_SHUTDOWN_GRACE_SECS") {
            self.shutdown_grace_secs = parse("DECANTER_REPORTS_SHUTDOWN_GRACE_SECS", &v)?;
        }
        if let Some(v) = env("DECANTER_REPORTS_LIMIT_FAILURES_PER_IP") {
            self.limits.failures_per_ip = parse("DECANTER_REPORTS_LIMIT_FAILURES_PER_IP", &v)?;
        }
        if let Some(v) = env("DECANTER_REPORTS_LIMIT_FAILURE_WINDOW_SECS") {
            self.limits.failure_window_secs = parse("DECANTER_REPORTS_LIMIT_FAILURE_WINDOW_SECS", &v)?;
        }
        if let Some(v) = env("DECANTER_REPORTS_LIMIT_PER_IP_HOUR") {
            self.limits.per_ip_hour = parse("DECANTER_REPORTS_LIMIT_PER_IP_HOUR", &v)?;
        }
        if let Some(v) = env("DECANTER_REPORTS_LIMIT_PER_IP_DAY") {
            self.limits.per_ip_day = parse("DECANTER_REPORTS_LIMIT_PER_IP_DAY", &v)?;
        }
        if let Some(v) = env("DECANTER_REPORTS_LIMIT_GLOBAL_HOUR") {
            self.limits.global_hour = parse("DECANTER_REPORTS_LIMIT_GLOBAL_HOUR", &v)?;
        }
        if let Some(v) = env("DECANTER_REPORTS_LIMIT_GET_PER_IP_MINUTE") {
            self.limits.get_per_ip_minute = parse("DECANTER_REPORTS_LIMIT_GET_PER_IP_MINUTE", &v)?;
        }
        self.storage.apply_env(env, parse::<bool>)
    }

    fn validate(&mut self) -> Result<(), ConfigError> {
        if let Some(header) = &self.client_ip_header {
            let lower = header.trim().to_ascii_lowercase();
            if HeaderName::from_bytes(lower.as_bytes()).is_err() {
                return err("client_ip_header is not a valid header name");
            }
            self.client_ip_header = Some(lower);
        }
        if self.client_ip_header.is_some() && self.trusted_proxies.is_empty() {
            return err("client_ip_header needs trusted_proxies: without them it is never read");
        }
        if self.retention_days == 0 {
            return err("retention_days must be at least 1");
        }
        if !(1..=100).contains(&self.page_size) {
            return err("page_size must be between 1 and 100");
        }
        if !(1..=1_000_000).contains(&self.max_connections) {
            return err("max_connections must be between 1 and 1000000");
        }
        if self.header_timeout_secs == 0 {
            return err("header_timeout_secs must be at least 1");
        }
        let limits = &self.limits;
        if limits.per_ip_hour == 0
            || limits.per_ip_day == 0
            || limits.global_hour == 0
            || limits.get_per_ip_minute == 0
            || limits.failures_per_ip == 0
            || limits.failure_window_secs == 0
        {
            return err("limits must be at least 1");
        }
        if limits.per_ip_day < limits.per_ip_hour {
            return err("limits.per_ip_day must not be below limits.per_ip_hour");
        }
        if self.public_url.ends_with('/') {
            return err("public_url must not end with a slash");
        }
        if self.log_level.parse::<LevelFilter>().is_err() {
            return err("log_level must be error, warn, info, debug, trace or off");
        }
        self.storage.validate()
    }

    /// Settings that are valid but let a client pick its own address for the per-address limits.
    ///
    /// `trusted_proxies` holds single addresses, not ranges, so a broad range cannot be written; an unspecified
    /// address is what someone typing `0.0.0.0/0` would end up wanting, and it never matches a peer.
    #[must_use]
    pub fn warnings(&self) -> Vec<String> {
        let mut warnings = Vec::new();
        let Some(header) = &self.client_ip_header else {
            return warnings;
        };
        if !self.listen.ip().is_loopback() {
            warnings.push(format!(
                "client_ip_header is set but listen is {}, not loopback: anything that can reach the port directly \
                 and comes from a trusted proxy address picks its own client address; bind to loopback or make sure \
                 only the tunnel reaches the port",
                self.listen
            ));
        }
        if self
            .trusted_proxies
            .iter()
            .any(|ip| ip.is_unspecified() || ip.is_multicast())
        {
            warnings.push(
                "trusted_proxies holds an unspecified or multicast address, which no connection comes from; list \
                 the tunnel's single address (ranges are not supported)"
                    .to_owned(),
            );
        }
        if matches!(header.as_str(), "x-forwarded-for" | "forwarded" | "x-real-ip") {
            warnings.push(format!(
                "client_ip_header {header} is a header clients can send and proxies append to: prefer a \
                 single-valued header the tunnel overwrites (for example cf-connecting-ip) and test it with a \
                 forged value through the real tunnel (docs/deploy.md)"
            ));
        }
        warnings
    }

    /// The key itself never appears in an error.
    ///
    /// # Errors
    ///
    /// No key is configured, it cannot be read, or it is shorter than 32 characters.
    pub fn admin_key(&self, env: &impl Fn(&str) -> Option<String>) -> Result<String, ConfigError> {
        let source = SecretSource {
            what: "the admin key",
            file_key: "admin_key_file",
            env_name: &self.admin_key_env,
            file: self.admin_key_file.as_deref(),
        };
        let key = source.read(env)?;
        if key.chars().count() < MIN_ADMIN_KEY_LEN {
            return err(format!(
                "the admin key must have at least {MIN_ADMIN_KEY_LEN} characters"
            ));
        }
        Ok(key)
    }

    /// Every secret the server needs; none of them ever appears in an error.
    ///
    /// # Errors
    ///
    /// A secret is missing, unreadable, readable by group or others, or too short.
    pub fn credentials(&self, env: &impl Fn(&str) -> Option<String>) -> Result<Credentials, ConfigError> {
        let storage = &self.storage;
        let passphrase = SecretSource {
            what: "the storage passphrase",
            file_key: "storage.passphrase_file",
            env_name: &storage.passphrase_env,
            file: storage.passphrase_file.as_deref(),
        }
        .read(env)?;
        if passphrase.chars().count() < MIN_PASSPHRASE_LEN {
            return err(format!(
                "the storage passphrase must have at least {MIN_PASSPHRASE_LEN} characters"
            ));
        }
        let bucket_keys = match storage.driver {
            Driver::Local => None,
            Driver::S3 | Driver::R2 => Some((
                SecretSource {
                    what: "the bucket access key id",
                    file_key: "storage.access_key_id_file",
                    env_name: &storage.access_key_id_env,
                    file: storage.access_key_id_file.as_deref(),
                }
                .read(env)?,
                SecretSource {
                    what: "the bucket secret access key",
                    file_key: "storage.secret_access_key_file",
                    env_name: &storage.secret_access_key_env,
                    file: storage.secret_access_key_file.as_deref(),
                }
                .read(env)?,
            )),
        };
        Ok(Credentials {
            admin_key: self.admin_key(env)?,
            passphrase,
            bucket_keys,
        })
    }
}

impl Storage {
    fn apply_env(
        &mut self,
        env: &impl Fn(&str) -> Option<String>,
        parse: fn(&str, &str) -> Result<bool, ConfigError>,
    ) -> Result<(), ConfigError> {
        if let Some(v) = env("DECANTER_REPORTS_STORAGE_DRIVER") {
            self.driver = v.parse()?;
        }
        if let Some(v) = env("DECANTER_REPORTS_STORAGE_DIR") {
            self.dir = PathBuf::from(v);
        }
        for (name, slot) in [
            ("DECANTER_REPORTS_STORAGE_BUCKET", &mut self.bucket),
            ("DECANTER_REPORTS_STORAGE_ENDPOINT", &mut self.endpoint),
            ("DECANTER_REPORTS_STORAGE_REGION", &mut self.region),
            ("DECANTER_REPORTS_STORAGE_ACCOUNT_ID", &mut self.account_id),
            ("DECANTER_REPORTS_STORAGE_PREFIX", &mut self.prefix),
            ("DECANTER_REPORTS_ACCESS_KEY_ID_ENV", &mut self.access_key_id_env),
            (
                "DECANTER_REPORTS_SECRET_ACCESS_KEY_ENV",
                &mut self.secret_access_key_env,
            ),
            ("DECANTER_REPORTS_PASSPHRASE_ENV", &mut self.passphrase_env),
        ] {
            if let Some(v) = env(name) {
                *slot = v;
            }
        }
        for (name, slot) in [
            ("DECANTER_REPORTS_ACCESS_KEY_ID_FILE", &mut self.access_key_id_file),
            (
                "DECANTER_REPORTS_SECRET_ACCESS_KEY_FILE",
                &mut self.secret_access_key_file,
            ),
            ("DECANTER_REPORTS_PASSPHRASE_FILE", &mut self.passphrase_file),
        ] {
            if let Some(v) = env(name) {
                *slot = Some(PathBuf::from(v));
            }
        }
        if let Some(v) = env("DECANTER_REPORTS_STORAGE_PATH_STYLE") {
            self.path_style = parse("DECANTER_REPORTS_STORAGE_PATH_STYLE", &v)?;
        }
        if let Some(v) = env("DECANTER_REPORTS_STORAGE_ALLOW_INSECURE_HTTP") {
            self.allow_insecure_http = parse("DECANTER_REPORTS_STORAGE_ALLOW_INSECURE_HTTP", &v)?;
        }
        Ok(())
    }

    fn validate(&self) -> Result<(), ConfigError> {
        let prefix_ok = self.prefix.is_empty()
            || self.prefix.split('/').all(|part| {
                !part.is_empty()
                    && !part.starts_with('.')
                    && part
                        .bytes()
                        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'-' | b'_' | b'.'))
            });
        if !prefix_ok {
            return err(
                "storage.prefix must be segments of a-z, 0-9, '-', '_', '.' joined by '/', none starting with '.'",
            );
        }
        match self.driver {
            Driver::Local => {
                if self.dir.as_os_str().is_empty() {
                    return err("storage.dir is required for the local driver");
                }
                if !self.prefix.is_empty() {
                    return err("storage.prefix is only for the s3 and r2 drivers; use storage.dir");
                }
            }
            Driver::S3 => {
                if self.bucket.is_empty() {
                    return err("storage.bucket is required for the s3 driver");
                }
                if !self.endpoint.is_empty() && !self.endpoint.starts_with("https://") {
                    let plain = self.endpoint.starts_with("http://");
                    if !plain {
                        return err("storage.endpoint must be an https:// (or http://) URL");
                    }
                    if !self.allow_insecure_http {
                        return err("storage.endpoint is plain http: use TLS, or set storage.allow_insecure_http");
                    }
                }
            }
            Driver::R2 => {
                if self.bucket.is_empty() || self.account_id.is_empty() {
                    return err("storage.bucket and storage.account_id are required for the r2 driver");
                }
            }
        }
        Ok(())
    }
}

/// Where one secret comes from: a file (systemd credential or owner-only) or, without one, an environment variable.
struct SecretSource<'a> {
    what: &'static str,
    file_key: &'static str,
    env_name: &'a str,
    file: Option<&'a Path>,
}

impl SecretSource<'_> {
    fn read(&self, env: &impl Fn(&str) -> Option<String>) -> Result<String, ConfigError> {
        let value = match self.file {
            Some(path) => self.read_file(path, env)?,
            None => match env(self.env_name).filter(|value| !value.trim().is_empty()) {
                Some(value) => value,
                None => {
                    return err(format!(
                        "{} is missing: set {} or {}",
                        self.what, self.env_name, self.file_key
                    ));
                }
            },
        };
        Ok(value.trim().to_owned())
    }

    fn read_file(&self, path: &Path, env: &impl Fn(&str) -> Option<String>) -> Result<String, ConfigError> {
        let (path, from_credentials) = match path.strip_prefix(CREDENTIALS_DIR_PREFIX) {
            Ok(rest) => {
                let Some(dir) = env("CREDENTIALS_DIRECTORY").filter(|dir| !dir.is_empty()) else {
                    return err(format!(
                        "{} starts with $CREDENTIALS_DIRECTORY but it is not set (LoadCredential=)",
                        self.file_key
                    ));
                };
                (Path::new(&dir).join(rest), true)
            }
            Err(_) => (path.to_path_buf(), false),
        };
        let unreadable =
            |error: std::io::Error| ConfigError(format!("cannot read {} from {}: {error}", self.what, path.display()));
        let mut file = std::fs::File::open(&path).map_err(unreadable)?;
        if !from_credentials && let Some(mode) = crate::perms::is_shared(&file).map_err(unreadable)? {
            return err(format!(
                "the file holding {} ({}) has mode {mode:04o}: group and others must have no access (chmod 600)",
                self.what,
                path.display()
            ));
        }
        let mut value = String::new();
        std::io::Read::read_to_string(&mut file, &mut value).map_err(unreadable)?;
        Ok(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn no_env(_: &str) -> Option<String> {
        None
    }

    #[test]
    fn defaults_listen_on_loopback() {
        let config = Config::load(None, &no_env).ok();
        assert_eq!(config.map(|c| c.listen), Some(SocketAddr::from(([127, 0, 0, 1], 8787))));
    }

    #[test]
    fn header_without_proxies_is_refused() {
        let env = |name: &str| (name == "DECANTER_REPORTS_CLIENT_IP_HEADER").then(|| "X-Forwarded-For".to_owned());
        assert!(Config::load(None, &env).is_err());
    }

    #[test]
    fn short_or_missing_admin_key_is_refused() {
        let config = Config::default();
        assert!(config.admin_key(&no_env).is_err());
        let short = |_: &str| Some("short".to_owned());
        assert!(config.admin_key(&short).is_err());
        let long = |_: &str| Some(format!("{}\n", "k".repeat(32)));
        assert_eq!(config.admin_key(&long).map(|k| k.len()).ok(), Some(32));
    }

    fn key_dir() -> Result<PathBuf, Box<dyn std::error::Error>> {
        let dir = std::env::temp_dir().join(format!("decanter-reports-key-{}", crate::secrets::random_hex()?));
        std::fs::create_dir_all(&dir)?;
        Ok(dir)
    }

    fn key_config(path: &str) -> Config {
        Config {
            admin_key_file: Some(PathBuf::from(path)),
            ..Config::default()
        }
    }

    #[test]
    fn key_file_readable_by_others_is_refused() -> Result<(), Box<dyn std::error::Error>> {
        use std::os::unix::fs::PermissionsExt;
        let dir = key_dir()?;
        let path = dir.join("admin.key");
        std::fs::write(&path, "k".repeat(32))?;
        let config = key_config(&path.to_string_lossy());
        for (mode, accepted) in [
            (0o644, false),
            (0o640, false),
            (0o604, false),
            (0o600, true),
            (0o400, true),
        ] {
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode))?;
            assert_eq!(config.admin_key(&no_env).is_ok(), accepted, "{mode:o}");
        }
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644))?;
        let message = config
            .admin_key(&no_env)
            .err()
            .map(|e| e.to_string())
            .unwrap_or_default();
        assert!(message.contains("0644") && !message.contains("kkkk"), "{message}");
        std::fs::remove_dir_all(&dir)?;
        Ok(())
    }

    #[test]
    fn credentials_directory_key_skips_the_mode_check() -> Result<(), Box<dyn std::error::Error>> {
        use std::os::unix::fs::PermissionsExt;
        let dir = key_dir()?;
        let path = dir.join("admin_key");
        std::fs::write(&path, "k".repeat(32))?;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644))?;
        let config = key_config("$CREDENTIALS_DIRECTORY/admin_key");
        let env = |name: &str| (name == "CREDENTIALS_DIRECTORY").then(|| dir.to_string_lossy().into_owned());
        assert_eq!(config.admin_key(&env).map(|k| k.len()).ok(), Some(32));
        assert!(config.admin_key(&no_env).is_err());
        std::fs::remove_dir_all(&dir)?;
        Ok(())
    }

    #[test]
    fn warnings_flag_a_public_listener_a_wildcard_proxy_and_a_forwardable_header() {
        let base = Config {
            client_ip_header: Some("cf-connecting-ip".to_owned()),
            trusted_proxies: vec![IpAddr::from([127, 0, 0, 1])],
            ..Config::default()
        };
        assert!(base.warnings().is_empty());
        assert!(Config::default().warnings().is_empty());
        let public = Config {
            listen: SocketAddr::from(([0, 0, 0, 0], 8787)),
            ..base.clone()
        };
        assert_eq!(public.warnings().len(), 1);
        let wildcard = Config {
            trusted_proxies: vec![IpAddr::from([0, 0, 0, 0])],
            ..base.clone()
        };
        assert!(wildcard.warnings().iter().any(|w| w.contains("unspecified")));
        let forwarded = Config {
            client_ip_header: Some("x-forwarded-for".to_owned()),
            ..base
        };
        assert!(forwarded.warnings().iter().any(|w| w.contains("single-valued")));
    }

    fn storage_env<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |name: &str| {
            pairs
                .iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| (*value).to_owned())
        }
    }

    #[test]
    fn storage_settings_are_checked_per_driver() {
        let s3 = [("DECANTER_REPORTS_STORAGE_DRIVER", "s3")];
        assert!(Config::load(None, &storage_env(&s3)).is_err(), "s3 needs a bucket");
        let plain = [
            ("DECANTER_REPORTS_STORAGE_DRIVER", "s3"),
            ("DECANTER_REPORTS_STORAGE_BUCKET", "reports"),
            ("DECANTER_REPORTS_STORAGE_ENDPOINT", "http://minio.lan:9000"),
        ];
        assert!(
            Config::load(None, &storage_env(&plain)).is_err(),
            "plain http needs the opt-in"
        );
        let opted = [
            plain.as_slice(),
            &[("DECANTER_REPORTS_STORAGE_ALLOW_INSECURE_HTTP", "true")],
        ]
        .concat();
        assert!(Config::load(None, &storage_env(&opted)).is_ok());
        for prefix in ["../x", "a//b", ".hidden", "Upper"] {
            let bad = [opted.as_slice(), &[("DECANTER_REPORTS_STORAGE_PREFIX", prefix)]].concat();
            assert!(Config::load(None, &storage_env(&bad)).is_err(), "{prefix}");
        }
        let good = [
            opted.as_slice(),
            &[("DECANTER_REPORTS_STORAGE_PREFIX", "decanter/reports")],
        ]
        .concat();
        assert!(Config::load(None, &storage_env(&good)).is_ok());
        let local_prefix = [("DECANTER_REPORTS_STORAGE_PREFIX", "x")];
        assert!(
            Config::load(None, &storage_env(&local_prefix)).is_err(),
            "local takes no prefix"
        );
        assert!(Config::load(None, &storage_env(&[("DECANTER_REPORTS_STORAGE_DRIVER", "gcs")])).is_err());
    }

    #[test]
    fn credentials_need_a_long_passphrase_and_bucket_keys_for_s3() -> Result<(), Box<dyn std::error::Error>> {
        use std::os::unix::fs::PermissionsExt;
        let admin = "k".repeat(32);
        let passphrase = "p".repeat(32);
        let local = Config::default();
        let short = [
            ("DECANTER_REPORTS_ADMIN_KEY", admin.as_str()),
            ("DECANTER_REPORTS_PASSPHRASE", "short"),
        ];
        let message = local
            .credentials(&storage_env(&short))
            .err()
            .map(|e| e.to_string())
            .unwrap_or_default();
        assert!(
            message.contains("passphrase") && !message.contains("short"),
            "{message}"
        );
        let both = [
            ("DECANTER_REPORTS_ADMIN_KEY", admin.as_str()),
            ("DECANTER_REPORTS_PASSPHRASE", passphrase.as_str()),
        ];
        assert!(
            local
                .credentials(&storage_env(&both))
                .is_ok_and(|c| c.bucket_keys.is_none())
        );

        let dir = key_dir()?;
        std::fs::write(dir.join("secret_access_key"), "s3-secret")?;
        std::fs::set_permissions(dir.join("secret_access_key"), std::fs::Permissions::from_mode(0o644))?;
        let mut s3 = Config::default();
        s3.storage.driver = Driver::S3;
        s3.storage.bucket = "reports".to_owned();
        assert!(s3.credentials(&storage_env(&both)).is_err(), "s3 needs its keys");
        s3.storage.secret_access_key_file = Some(dir.join("secret_access_key"));
        let with_id = [both.as_slice(), &[("DECANTER_REPORTS_S3_ACCESS_KEY_ID", "AKIA")]].concat();
        let message = s3
            .credentials(&storage_env(&with_id))
            .err()
            .map(|e| e.to_string())
            .unwrap_or_default();
        assert!(message.contains("0644") && !message.contains("s3-secret"), "{message}");
        s3.storage.secret_access_key_file = Some(PathBuf::from("$CREDENTIALS_DIRECTORY/secret_access_key"));
        let dir_text = dir.to_string_lossy().into_owned();
        let systemd = [with_id.as_slice(), &[("CREDENTIALS_DIRECTORY", dir_text.as_str())]].concat();
        let keys = s3.credentials(&storage_env(&systemd))?.bucket_keys;
        assert_eq!(keys, Some(("AKIA".to_owned(), "s3-secret".to_owned())));
        std::fs::remove_dir_all(&dir)?;
        Ok(())
    }
}
