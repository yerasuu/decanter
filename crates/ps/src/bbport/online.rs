//! psport's online play (decision #715): the settings decanter keeps for it, checked before they
//! become the variables `run.sh` reads, the title's own online data, and the connection test.
//!
//! decanter launches through `run.sh`, which does the rest for `PS_ONLINE=1` as psport's own
//! launcher relies on: CA certificates, `--online-overrides`, `scripts/online.py`'s host overrides
//! and the title's local game server. The shadNet password and token never reach the settings
//! file: the caller takes them from the keyring and hands them over for one launch.

use std::collections::BTreeMap;
use std::time::Duration;

use decanter_nix::Toolchain;

use super::{Bbport, LauncherSettings, Names};
use crate::os::linux::toolchain;
use crate::port::Port;

/// psport's launcher's defaults (`scripts/online.py`): Seamless Hunt's servers.
pub const SHADNET_SERVER: &str = "play.seamlesshunt.com:31313";
pub const COMMUNITY_SERVER: &str = "http://play.seamlesshunt.com";
const SHADNET_PORT: u16 = 31313;
const WEBAPI_PORT: u16 = 31315;

/// Launch settings keys, as psport's launcher names them so its file imports as is. Its
/// `online_password` is never read nor kept ([`LauncherSettings::save`] drops it).
pub const ONLINE: &str = "online";
pub const SERVER: &str = "online_server";
pub const WEBAPI: &str = "online_webapi";
pub const COMMUNITY: &str = "online_community";
pub const NPID: &str = "online_npid";
pub const UPNP: &str = "online_upnp";
pub const BBSC: &str = "online_bbsc";
pub const BBSC_LOCAL_SERVER: &str = "online_bbsc_local_server";
pub const TLS: &str = "online_tls";
pub const ALLOW_PLAINTEXT: &str = "online_allow_plaintext";

/// The co-op extension whose switches the Bloodborne page shows.
pub const BBSC_EXTENSION: &str = "bbsc";

const MAX_HOST: usize = 253;
const MAX_ADDRESS: usize = 512;
const MAX_SECRET: usize = 1024;
const MAX_TITLE_ONLINE: usize = 16 * 1024;
const MAX_PROBE_BODY: usize = 512;

/// Why an online setting can't be used, in technical English; the app words it for the UI.
#[derive(Debug, thiserror::Error)]
pub enum OnlineError {
    #[error("shadNet server {0:?} is not host:port")]
    Server(String),
    #[error("community server {0:?} is not an http(s) host")]
    Community(String),
    #[error("WebAPI {0:?} is not an http(s) address")]
    Webapi(String),
    #[error("NPID looks like an email address")]
    NpidEmail,
    #[error("NPID {0:?} is not 3 to 16 letters, digits, '-' or '_'")]
    Npid(String),
    #[error("shadNet password or token unusable in the environment")]
    Secret,
}

/// Whether the title's co-op extension runs its game server on this PC (`PS_BBSC_LOCAL_SERVER`).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum LocalServer {
    #[default]
    Off,
    On,
    /// Only when the community game server doesn't answer.
    Auto,
}

impl LocalServer {
    pub const ALL: [Self; 3] = [Self::Off, Self::On, Self::Auto];

    /// As kept in the settings and passed to `run.sh`.
    pub const fn value(self) -> &'static str {
        match self {
            Self::Off => "0",
            Self::On => "1",
            Self::Auto => "auto",
        }
    }

    pub fn of(settings: &LauncherSettings) -> Self {
        let value = settings.text(BBSC_LOCAL_SERVER).unwrap_or_default();
        Self::ALL
            .into_iter()
            .find(|mode| mode.value() == value)
            .unwrap_or_default()
    }
}

fn is_host(host: &str) -> bool {
    !host.is_empty()
        && host.len() <= MAX_HOST
        && !host.starts_with(['-', '.'])
        && host
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '.' || ch == '-')
}

/// `scripts/online.py`'s `community_address`: `scheme://host`, the game picks the ports. Without
/// a scheme https, except the default's own host, which serves plain http.
pub fn community_address(value: &str) -> String {
    let value = Some(value.trim())
        .filter(|value| !value.is_empty())
        .unwrap_or(COMMUNITY_SERVER);
    let (scheme, rest) = value.rsplit_once("://").unwrap_or(("", value));
    let host = rest.split('/').next().unwrap_or_default();
    let host = host.split(':').next().unwrap_or_default();
    let (default_scheme, default_host) = COMMUNITY_SERVER.split_once("://").unwrap_or(("http", COMMUNITY_SERVER));
    let scheme = match scheme {
        "" if host == default_host => default_scheme,
        "" => "https",
        scheme => scheme,
    };
    format!("{scheme}://{host}")
}

/// The shadNet server as `host:port`, the server's own port when none is given.
fn server_address(value: &str) -> Result<String, OnlineError> {
    let value = Some(value.trim())
        .filter(|value| !value.is_empty())
        .unwrap_or(SHADNET_SERVER);
    let (host, port) = match value.rsplit_once(':') {
        Some((host, port)) => (host, port.parse::<u16>().ok().filter(|&port| port > 0)),
        None => (value, Some(SHADNET_PORT)),
    };
    match port {
        Some(port) if is_host(host) => Ok(format!("{host}:{port}")),
        _ => Err(OnlineError::Server(value.to_owned())),
    }
}

/// `scripts/online.py`'s `webapi_setting`: the field, unless it holds the community server's
/// host (a common mix-up), else the shadNet server's own WebAPI on its default port.
fn webapi_address(value: &str, server: &str, community: &str) -> String {
    let value = value.trim();
    let community_host = community.split_once("://").map_or(community, |(_, host)| host);
    if value.is_empty() || value.contains(community_host) {
        let host = server.rsplit_once(':').map_or(server, |(host, _)| host);
        format!("http://{host}:{WEBAPI_PORT}")
    } else {
        value.to_owned()
    }
}

/// `address` as the URL parser reads it, which is what the port then gets: the parser drops
/// characters a raw string would carry on. A lone `/` path stays off, as it was typed.
fn http_address(address: &str) -> Option<String> {
    let url = reqwest::Url::parse(address).ok()?;
    let plain =
        address.len() <= MAX_ADDRESS && matches!(url.scheme(), "http" | "https") && url.host_str().is_some_and(is_host);
    let bare = url.path() == "/" && url.query().is_none() && url.fragment().is_none();
    plain.then(|| {
        let text = url.as_str();
        if bare { text.trim_end_matches('/') } else { text }.to_owned()
    })
}

/// A field holding a control character is refused whole rather than cleaned up.
fn has_control(text: &str) -> bool {
    text.chars().any(char::is_control)
}

/// The three servers online play talks to, normalised as psport's launcher does and checked.
#[derive(Clone, Debug)]
pub struct Addresses {
    /// `host:port`.
    pub server: String,
    /// `http(s)://host[:port][/path]`.
    pub webapi: String,
    /// `http(s)://host`.
    pub community: String,
}

impl Addresses {
    /// From the fields as typed; empty ones take the defaults.
    pub fn new(server: &str, webapi: &str, community: &str) -> Result<Self, OnlineError> {
        if has_control(server) {
            return Err(OnlineError::Server(server.escape_default().to_string()));
        }
        if has_control(community) {
            return Err(OnlineError::Community(community.escape_default().to_string()));
        }
        if has_control(webapi) {
            return Err(OnlineError::Webapi(webapi.escape_default().to_string()));
        }
        let server = server_address(server)?;
        let community = community_address(community);
        let Some(community) = http_address(&community) else {
            return Err(OnlineError::Community(community));
        };
        let webapi = webapi_address(webapi, &server, &community);
        let Some(webapi) = http_address(&webapi) else {
            return Err(OnlineError::Webapi(webapi));
        };
        Ok(Self {
            server,
            webapi,
            community,
        })
    }

    pub fn of(settings: &LauncherSettings) -> Result<Self, OnlineError> {
        let text = |key| settings.text(key).unwrap_or_default();
        Self::new(text(SERVER), text(WEBAPI), text(COMMUNITY))
    }
}

/// Whether `server` (`host:port`) is this machine, the only peer psport logs in to without TLS or
/// the plain-text switch; a name is judged by its spelling, as the port resolves it itself.
pub fn is_loopback_server(server: &str) -> bool {
    let host = server.rsplit_once(':').map_or(server, |(host, _)| host);
    host.eq_ignore_ascii_case("localhost") || host.starts_with("127.") || host == "[::1]"
}

/// The shadNet account name: what the server registered, never an email address. Empty is no
/// account, which psport takes for playing without shadNet.
pub fn check_npid(npid: &str) -> Result<Option<&str>, OnlineError> {
    if has_control(npid) {
        return Err(OnlineError::Npid(npid.escape_default().to_string()));
    }
    let npid = npid.trim();
    if npid.is_empty() {
        return Ok(None);
    }
    if npid.contains('@') {
        return Err(OnlineError::NpidEmail);
    }
    let plain = (3..=16).contains(&npid.len())
        && npid
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_');
    if plain {
        Ok(Some(npid))
    } else {
        Err(OnlineError::Npid(npid.to_owned()))
    }
}

fn check_secret(secret: Option<String>) -> Result<Option<String>, OnlineError> {
    match secret {
        Some(secret) if secret.len() > MAX_SECRET || secret.contains('\0') => Err(OnlineError::Secret),
        secret => Ok(secret.filter(|secret| !secret.is_empty())),
    }
}

/// Variables never written out with a run's command, since they hold the shadNet secrets.
pub fn is_secret_var(name: &str) -> bool {
    name.ends_with("SHADNET_PASSWORD") || name.ends_with("SHADNET_TOKEN")
}

/// One launch's online play, checked. No `Debug`: it holds the shadNet secrets.
pub struct Online {
    addresses: Addresses,
    npid: Option<String>,
    password: Option<String>,
    token: Option<String>,
    upnp: bool,
    bbsc: bool,
    local_server: LocalServer,
    tls: bool,
    allow_plaintext: bool,
}

impl Online {
    /// `None` while online play is off, the default.
    pub fn from_settings(settings: &LauncherSettings) -> Result<Option<Self>, OnlineError> {
        if !settings.flag(ONLINE, false) {
            return Ok(None);
        }
        let addresses = Addresses::of(settings)?;
        let npid = check_npid(settings.text(NPID).unwrap_or_default())?.map(str::to_owned);
        Ok(Some(Self {
            addresses,
            npid,
            password: None,
            token: None,
            upnp: settings.flag(UPNP, true),
            bbsc: settings.flag(BBSC, false),
            local_server: LocalServer::of(settings),
            tls: settings.flag(TLS, false),
            allow_plaintext: settings.flag(ALLOW_PLAINTEXT, true),
        }))
    }

    /// Whose secrets the launch needs.
    pub fn npid(&self) -> Option<&str> {
        self.npid.as_deref()
    }

    pub fn set_secrets(&mut self, password: Option<String>, token: Option<String>) -> Result<(), OnlineError> {
        self.password = check_secret(password)?;
        self.token = check_secret(token)?;
        Ok(())
    }

    /// What `run.sh` reads for online play, spelled for the port at hand.
    pub(super) fn env(&self, names: Names) -> Vec<(String, &str)> {
        let switch = |on: bool| if on { "1" } else { "0" };
        let mut env = vec![
            (names.var("ONLINE"), "1"),
            (names.var("SHADNET_SERVER"), self.addresses.server.as_str()),
            (names.var("SHADNET_WEBAPI"), self.addresses.webapi.as_str()),
            (names.var("COMMUNITY_SERVER"), self.addresses.community.as_str()),
            (names.var("UPNP"), switch(self.upnp)),
            (names.var("BBSC"), switch(self.bbsc)),
            (names.var("BBSC_LOCAL_SERVER"), self.local_server.value()),
        ];
        env.extend(
            [
                ("SHADNET_TLS", self.tls),
                ("ONLINE_ALLOW_PLAINTEXT_LOGIN", self.allow_plaintext),
            ]
            .into_iter()
            .filter(|&(_, on)| on)
            .map(|(suffix, _)| (names.var(suffix), "1")),
        );
        let account = [
            ("SHADNET_NPID", self.npid.as_deref()),
            ("SHADNET_PASSWORD", self.password.as_deref()),
            ("SHADNET_TOKEN", self.token.as_deref()),
        ];
        env.extend(
            account
                .into_iter()
                .filter_map(|(suffix, value)| value.map(|value| (names.var(suffix), value))),
        );
        env
    }
}

/// What `psport --online-overrides` says of a title: its servers and its co-op extension.
pub struct TitleOnline(BTreeMap<String, String>);

impl TitleOnline {
    /// The co-op extension's name, such as [`BBSC_EXTENSION`].
    pub fn extension(&self) -> Option<&str> {
        self.0.get("extension").map(String::as_str)
    }

    /// The edge where the port's answer becomes decanter's data: `key=value` lines, plain keys.
    fn parse(stdout: &[u8]) -> Result<Self, String> {
        if stdout.len() > MAX_TITLE_ONLINE {
            return Err(format!("answer larger than {MAX_TITLE_ONLINE} bytes"));
        }
        let text = std::str::from_utf8(stdout).map_err(|err| err.to_string())?;
        let mut values = BTreeMap::new();
        for line in text.lines().filter(|line| !line.trim().is_empty()) {
            let Some((key, value)) = line.split_once('=') else {
                return Err(format!("line without '=': {line:?}"));
            };
            if key.is_empty() || !key.chars().all(|ch| ch.is_ascii_lowercase() || ch == '_') {
                return Err(format!("malformed key {key:?}"));
            }
            values.insert(key.to_owned(), value.trim().to_owned());
        }
        Ok(Self(values))
    }
}

/// `None` when the port can't say: not built, or a release from before the query.
pub async fn title_online(bbport: &Bbport, toolchain: &Toolchain, title_id: &str) -> Option<TitleOnline> {
    let names = Names::of(&bbport.host_port_dir(toolchain).await).await;
    if !matches!(names, Names::Psport) {
        return None;
    }
    let script = format!(
        r#"exec "$DECANTER_PORT_DIR/{}" --online-overrides --title "$DECANTER_TITLE""#,
        names.probe()
    );
    let mut command = match toolchain::command(toolchain, &bbport.root, Port::Bbport, &script).await {
        Ok(command) => command,
        Err(err) => {
            tracing::warn!(%err, "title online data not queried");
            return None;
        }
    };
    command
        .env("DECANTER_PORT_DIR", bbport.port_dir().await)
        .env("DECANTER_TITLE", title_id);
    let (status, stdout) = match super::query_port(command, MAX_TITLE_ONLINE).await {
        Ok(answer) => answer,
        Err(err) => {
            tracing::warn!(%err, "title online data not queried");
            return None;
        }
    };
    if !status.success() {
        tracing::debug!(title_id, %status, "port gives no online data for the title");
        return None;
    }
    match TitleOnline::parse(&stdout) {
        Ok(online) => {
            tracing::debug!(title_id, extension = online.extension(), "title online data read");
            Some(online)
        }
        Err(err) => {
            tracing::warn!(title_id, %err, "title online data doesn't check out, ignoring it");
            None
        }
    }
}

/// A server the connection test reached for, as `scripts/online.py check` names them.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Target {
    /// The community game server: messages, bloodstains, ghosts.
    Game,
    Shadnet,
    Webapi,
}

pub struct Probe {
    pub target: Target,
    pub address: String,
    /// Why it failed, in the words of whatever failed.
    pub outcome: Result<(), String>,
}

/// `scripts/online.py check`, each step bounded in time.
///
/// The community game server's status file, a TCP connection to the shadNet server and its
/// WebAPI's status. Only ever run on the user's click (decision #601: nothing automatic connects).
pub async fn check(addresses: &Addresses) -> [Probe; 3] {
    tracing::info!(
        server = %addresses.server,
        webapi = %addresses.webapi,
        community = %addresses.community,
        "testing the online servers"
    );
    let game = format!("{}:20443/bb-eu/ss.info", addresses.community);
    let status = format!("{}/status", addresses.webapi.trim_end_matches('/'));
    let (game, shadnet, webapi) = tokio::join!(
        get(&game, Duration::from_secs(6)),
        connect(&addresses.server),
        get(&status, Duration::from_secs(4)),
    );
    let webapi = webapi.and_then(|body| {
        let body: Vec<u8> = body.into_iter().filter(|&byte| byte != b' ').collect();
        if body.windows(9).any(|window| window == b"\"ok\":true") {
            Ok(())
        } else {
            Err("not a shadNet server".to_owned())
        }
    });
    let probes = [
        Probe {
            target: Target::Game,
            address: addresses.community.clone(),
            outcome: game.map(drop),
        },
        Probe {
            target: Target::Shadnet,
            address: addresses.server.clone(),
            outcome: shadnet,
        },
        Probe {
            target: Target::Webapi,
            address: addresses.webapi.clone(),
            outcome: webapi,
        },
    ];
    probes.iter().for_each(log_probe);
    probes
}

fn log_probe(probe: &Probe) {
    match &probe.outcome {
        Ok(()) => tracing::info!(target = ?probe.target, address = %probe.address, "online server answers"),
        Err(err) => {
            tracing::warn!(target = ?probe.target, address = %probe.address, %err, "online server doesn't answer");
        }
    }
}

/// The first [`MAX_PROBE_BODY`] bytes of `url`'s answer, as the script reads no more of it.
async fn get(url: &str, timeout: Duration) -> Result<Vec<u8>, String> {
    let client = reqwest::Client::builder()
        .timeout(timeout)
        .build()
        .map_err(|err| err.to_string())?;
    let mut response = client
        .get(url)
        .send()
        .await
        .and_then(reqwest::Response::error_for_status)
        .map_err(|err| err.to_string())?;
    let mut body = Vec::with_capacity(MAX_PROBE_BODY);
    while body.len() < MAX_PROBE_BODY {
        match response.chunk().await {
            Ok(Some(chunk)) => body.extend_from_slice(&chunk),
            Ok(None) => break,
            Err(err) => return Err(err.to_string()),
        }
    }
    body.truncate(MAX_PROBE_BODY);
    Ok(body)
}

async fn connect(server: &str) -> Result<(), String> {
    match tokio::time::timeout(Duration::from_secs(4), tokio::net::TcpStream::connect(server)).await {
        Ok(Ok(_)) => Ok(()),
        Ok(Err(err)) => Err(err.to_string()),
        Err(_) => Err("timed out".to_owned()),
    }
}
