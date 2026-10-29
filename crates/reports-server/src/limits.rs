//! Rate limits kept in memory only: an address lives here as a counter key and is gone on restart.

use std::collections::{HashMap, VecDeque};
use std::net::{IpAddr, Ipv6Addr, SocketAddr};
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

use axum::http::{HeaderMap, HeaderName};

use crate::config::{Config, Limits};

const MINUTE: Duration = Duration::from_mins(1);
const HOUR: Duration = Duration::from_hours(1);
const DAY: Duration = Duration::from_hours(24);
/// Bounds the table against an attacker rotating addresses; past it new addresses are refused until old ones expire.
const MAX_TRACKED: usize = 100_000;

pub enum Verdict {
    Allowed,
    Denied { retry_after: Duration },
}

#[derive(Default)]
struct State {
    per_ip: HashMap<IpAddr, VecDeque<Instant>>,
    global: VecDeque<Instant>,
    reads: HashMap<IpAddr, VecDeque<Instant>>,
    failures: HashMap<IpAddr, VecDeque<Instant>>,
}

pub struct Limiter {
    limits: Limits,
    state: Mutex<State>,
}

/// Octets kept of an IPv6 address on the upload path: a /48 is what a site is normally given, so rotating inside it
/// must not buy more uploads.
const UPLOAD_PREFIX: usize = 6;
/// Page reads are keyed by /64 so that one household behind a /48 does not share a browsing budget.
const READ_PREFIX: usize = 8;

/// An IPv6 client owns a whole prefix, so the prefix is the key; otherwise rotating inside it would be free. An
/// IPv4-mapped address (`::ffff:a.b.c.d`, what a dual-stack listener reports for IPv4 clients) is the IPv4 client,
/// since its /48 would hold every IPv4 address.
fn key(ip: IpAddr, prefix_octets: usize) -> IpAddr {
    match ip.to_canonical() {
        IpAddr::V6(v6) => {
            let mut octets = v6.octets();
            octets[prefix_octets..].fill(0);
            IpAddr::V6(Ipv6Addr::from(octets))
        }
        v4 @ IpAddr::V4(_) => v4,
    }
}

fn expire(queue: &mut VecDeque<Instant>, now: Instant, window: Duration) {
    while queue.front().is_some_and(|at| now.duration_since(*at) >= window) {
        queue.pop_front();
    }
}

/// How long until the entry that holds the queue at `limit` leaves the window; `None` when under the limit.
fn wait(queue: &VecDeque<Instant>, limit: u32, now: Instant, window: Duration) -> Option<Duration> {
    let at = queue.get(queue.len().checked_sub(limit as usize)?)?;
    (*at + window)
        .checked_duration_since(now)
        .filter(|left| !left.is_zero())
}

impl Limiter {
    #[must_use]
    pub fn new(limits: Limits) -> Self {
        Self {
            limits,
            state: Mutex::new(State::default()),
        }
    }

    /// Counts one request against the address's hourly and daily buckets, whatever becomes of the request, and
    /// refuses it while the global bucket is full. The global bucket is spent by `spend_global`, not here: a
    /// rejected upload must not use up what real players share.
    pub fn check(&self, ip: IpAddr, now: Instant) -> Verdict {
        let mut guard = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let verdict = self.decide(&mut guard, ip, now);
        drop(guard);
        verdict
    }

    /// The page-read bucket, apart from the upload one so browsing never spends an upload and the reverse.
    /// Past `MAX_TRACKED` addresses new ones go untracked rather than refused: a page read is cheap (the index is
    /// cached) and locking every reader out would serve the attacker's goal.
    pub fn check_read(&self, ip: IpAddr, now: Instant) -> Verdict {
        let mut guard = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let verdict = self.decide_read(&mut guard.reads, ip, now);
        drop(guard);
        verdict
    }

    fn decide_read(&self, reads: &mut HashMap<IpAddr, VecDeque<Instant>>, ip: IpAddr, now: Instant) -> Verdict {
        if reads.len() >= MAX_TRACKED {
            reads.retain(|_, queue| {
                expire(queue, now, MINUTE);
                !queue.is_empty()
            });
        }
        let ip = key(ip, READ_PREFIX);
        if reads.len() >= MAX_TRACKED && !reads.contains_key(&ip) {
            return Verdict::Allowed;
        }
        let queue = reads.entry(ip).or_default();
        expire(queue, now, MINUTE);
        if let Some(retry_after) = wait(queue, self.limits.get_per_ip_minute, now, MINUTE) {
            return Verdict::Denied { retry_after };
        }
        queue.push_back(now);
        Verdict::Allowed
    }

    fn decide(&self, state: &mut State, ip: IpAddr, now: Instant) -> Verdict {
        expire(&mut state.global, now, HOUR);
        if state.per_ip.len() >= MAX_TRACKED {
            state.per_ip.retain(|_, queue| {
                expire(queue, now, DAY);
                !queue.is_empty()
            });
        }
        let ip = key(ip, UPLOAD_PREFIX);
        if state.per_ip.len() >= MAX_TRACKED && !state.per_ip.contains_key(&ip) {
            return Verdict::Denied { retry_after: HOUR };
        }
        let queue = state.per_ip.entry(ip).or_default();
        expire(queue, now, DAY);
        let denial = [
            wait(queue, self.limits.per_ip_hour, now, HOUR),
            wait(queue, self.limits.per_ip_day, now, DAY),
            wait(&state.global, self.limits.global_hour, now, HOUR),
        ]
        .into_iter()
        .flatten()
        .max();
        if let Some(retry_after) = denial {
            return Verdict::Denied { retry_after };
        }
        queue.push_back(now);
        Verdict::Allowed
    }

    /// Whether the address has used up its authentication failures (wrong admin key, wrong delete code). Only
    /// failures count, so a maintainer or owner who gets it right is never slowed down by their own successes.
    pub fn check_failures(&self, ip: IpAddr, now: Instant) -> Verdict {
        let window = self.failure_window();
        let mut guard = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let verdict = guard.failures.get_mut(&key(ip, UPLOAD_PREFIX)).and_then(|queue| {
            expire(queue, now, window);
            wait(queue, self.limits.failures_per_ip, now, window)
        });
        drop(guard);
        verdict.map_or(Verdict::Allowed, |retry_after| Verdict::Denied { retry_after })
    }

    pub fn record_failure(&self, ip: IpAddr, now: Instant) {
        let window = self.failure_window();
        let mut guard = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let failures = &mut guard.failures;
        if failures.len() >= MAX_TRACKED {
            failures.retain(|_, queue| {
                expire(queue, now, window);
                !queue.is_empty()
            });
        }
        let ip = key(ip, UPLOAD_PREFIX);
        if failures.len() < MAX_TRACKED || failures.contains_key(&ip) {
            failures.entry(ip).or_default().push_back(now);
        }
        drop(guard);
    }

    fn failure_window(&self) -> Duration {
        Duration::from_secs(self.limits.failure_window_secs.into())
    }

    /// Spends one slot of the global bucket for a report that passed validation (or an attachment whose owner
    /// proved the delete code).
    pub fn spend_global(&self, now: Instant) {
        let mut guard = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        expire(&mut guard.global, now, HOUR);
        guard.global.push_back(now);
        drop(guard);
    }
}

/// How often the server may say that `client_ip_header` came from an untrusted peer, so a drift is visible in the
/// log without one line per request.
const IGNORED_HEADER_EVERY: Duration = Duration::from_mins(10);
static IGNORED_HEADER_SAID: Mutex<Option<Instant>> = Mutex::new(None);

/// Whether the drift warning is due now, and if so marks it said.
fn due(last: &Mutex<Option<Instant>>, now: Instant, every: Duration) -> bool {
    let mut last = last.lock().unwrap_or_else(PoisonError::into_inner);
    let due = last.is_none_or(|at| now.saturating_duration_since(at) >= every);
    if due {
        *last = Some(now);
    }
    drop(last);
    due
}

fn is_trusted(config: &Config, ip: IpAddr) -> bool {
    config.trusted_proxies.iter().any(|proxy| proxy.to_canonical() == ip)
}

/// The address a limit applies to.
///
/// The socket's address, unless it is a trusted proxy and the configured header names the client:
/// anyone else can write that header, so it counts for nothing from them. For a list (`X-Forwarded-For`)
/// the client is the rightmost entry that is not itself a trusted proxy, since each proxy appends the
/// address it saw and only the entries after the last trusted hop are not forgeable by the client.
/// Addresses are compared in their canonical form, so a dual-stack listener's `::ffff:a.b.c.d` is `a.b.c.d`.
#[must_use]
pub fn client_ip(peer: SocketAddr, headers: &HeaderMap, config: &Config) -> IpAddr {
    let peer_ip = peer.ip().to_canonical();
    let Some(name) = config
        .client_ip_header
        .as_deref()
        .and_then(|n| HeaderName::from_bytes(n.as_bytes()).ok())
    else {
        return peer_ip;
    };
    if !is_trusted(config, peer_ip) {
        // When the proxy's address drifts away from `trusted_proxies`, every client is counted as the proxy and
        // one client's failures lock everyone out of /admin; this line is the only sign of it.
        if headers.contains_key(&name) && due(&IGNORED_HEADER_SAID, Instant::now(), IGNORED_HEADER_EVERY) {
            tracing::warn!(
                target: "decanter_reports::limits",
                peer = %peer_ip,
                header = %name,
                "client_ip_header ignored: the peer is not in trusted_proxies, so the limits count the peer's \
                 address; if the peer is the proxy, its address changed"
            );
        }
        return peer_ip;
    }
    headers
        .get_all(name)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(','))
        .rev()
        .map(str::trim)
        .map(|text| text.parse::<IpAddr>().map(|ip| ip.to_canonical()))
        .find(|parsed| parsed.as_ref().map_or(true, |ip| !is_trusted(config, *ip)))
        .and_then(Result::ok)
        .unwrap_or(peer_ip)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn limiter(per_hour: u32, per_day: u32, global: u32) -> Limiter {
        Limiter::new(Limits {
            per_ip_hour: per_hour,
            per_ip_day: per_day,
            global_hour: global,
            ..Limits::default()
        })
    }

    fn denied_wait(verdict: &Verdict) -> Option<Duration> {
        match verdict {
            Verdict::Denied { retry_after } => Some(*retry_after),
            Verdict::Allowed => None,
        }
    }

    fn allowed(verdict: &Verdict) -> bool {
        matches!(verdict, Verdict::Allowed)
    }

    #[test]
    fn hour_limit_denies_with_a_wait_and_frees_later() {
        let limiter = limiter(2, 10, 100);
        let ip: IpAddr = "203.0.113.5".parse().unwrap_or_else(|_| IpAddr::from([0, 0, 0, 0]));
        let t0 = Instant::now();
        assert!(allowed(&limiter.check(ip, t0)));
        assert!(allowed(&limiter.check(ip, t0 + Duration::from_secs(60))));
        let verdict = limiter.check(ip, t0 + Duration::from_secs(120));
        assert_eq!(denied_wait(&verdict), Some(Duration::from_mins(58)));
        assert!(allowed(&limiter.check(ip, t0 + HOUR + Duration::from_secs(1))));
    }

    #[test]
    fn day_limit_outlasts_the_hour_window() {
        let limiter = limiter(10, 2, 100);
        let ip = IpAddr::from([198, 51, 100, 1]);
        let t0 = Instant::now();
        assert!(allowed(&limiter.check(ip, t0)));
        assert!(allowed(&limiter.check(ip, t0 + HOUR * 2)));
        assert!(!allowed(&limiter.check(ip, t0 + HOUR * 3)));
        assert!(allowed(&limiter.check(ip, t0 + DAY + Duration::from_secs(1))));
    }

    #[test]
    fn global_cap_applies_across_addresses() {
        let limiter = limiter(10, 10, 2);
        let t0 = Instant::now();
        for last in 1..=2 {
            assert!(allowed(&limiter.check(IpAddr::from([198, 51, 100, last]), t0)));
            limiter.spend_global(t0);
        }
        assert!(!allowed(&limiter.check(IpAddr::from([198, 51, 100, 3]), t0)));
        assert!(allowed(&limiter.check(IpAddr::from([198, 51, 100, 3]), t0 + HOUR)));
    }

    #[test]
    fn checks_alone_never_spend_the_global_cap() {
        let limiter = limiter(100, 100, 2);
        let t0 = Instant::now();
        for last in 0..50 {
            assert!(allowed(&limiter.check(IpAddr::from([198, 51, 100, last]), t0)));
        }
    }

    #[test]
    fn ipv6_uploads_count_per_48() {
        let limiter = limiter(1, 5, 100);
        let t0 = Instant::now();
        let parse = |text: &str| text.parse::<IpAddr>().unwrap_or_else(|_| IpAddr::from([0, 0, 0, 0]));
        assert!(allowed(&limiter.check(parse("2001:db8:1:2::1"), t0)));
        assert!(!allowed(&limiter.check(parse("2001:db8:1:2:aaaa::9"), t0)));
        assert!(!allowed(&limiter.check(parse("2001:db8:1:ffff::9"), t0)));
        assert!(allowed(&limiter.check(parse("2001:db8:2:2::1"), t0)));
    }

    #[test]
    fn ipv6_reads_count_per_64() {
        let limiter = Limiter::new(Limits {
            get_per_ip_minute: 1,
            ..Limits::default()
        });
        let t0 = Instant::now();
        let parse = |text: &str| text.parse::<IpAddr>().unwrap_or_else(|_| IpAddr::from([0, 0, 0, 0]));
        assert!(allowed(&limiter.check_read(parse("2001:db8:1:2::1"), t0)));
        assert!(!allowed(&limiter.check_read(parse("2001:db8:1:2:aaaa::9"), t0)));
        assert!(allowed(&limiter.check_read(parse("2001:db8:1:3::1"), t0)));
    }

    #[test]
    fn reads_have_their_own_bucket() {
        let limiter = Limiter::new(Limits {
            get_per_ip_minute: 2,
            per_ip_hour: 1,
            ..Limits::default()
        });
        let ip = IpAddr::from([198, 51, 100, 1]);
        let t0 = Instant::now();
        assert!(allowed(&limiter.check_read(ip, t0)));
        assert!(allowed(&limiter.check_read(ip, t0)));
        assert_eq!(denied_wait(&limiter.check_read(ip, t0)), Some(Duration::from_mins(1)));
        assert!(allowed(&limiter.check(ip, t0)));
        assert!(allowed(&limiter.check_read(ip, t0 + MINUTE)));
    }

    fn config_with(proxies: &[&str]) -> Config {
        Config {
            client_ip_header: Some("x-forwarded-for".to_owned()),
            trusted_proxies: proxies.iter().filter_map(|p| p.parse().ok()).collect(),
            ..Config::default()
        }
    }

    fn headers(value: &str) -> HeaderMap {
        let mut map = HeaderMap::new();
        if let Ok(value) = value.parse() {
            map.insert("x-forwarded-for", value);
        }
        map
    }

    #[test]
    fn header_is_ignored_unless_the_peer_is_a_trusted_proxy() {
        let config = config_with(&["10.0.0.1"]);
        let stranger: SocketAddr = "192.0.2.9:1000"
            .parse()
            .unwrap_or_else(|_| SocketAddr::from(([0, 0, 0, 0], 0)));
        assert_eq!(
            client_ip(stranger, &headers("1.2.3.4"), &config).to_string(),
            "192.0.2.9"
        );
        let proxy: SocketAddr = "10.0.0.1:1000"
            .parse()
            .unwrap_or_else(|_| SocketAddr::from(([0, 0, 0, 0], 0)));
        assert_eq!(client_ip(proxy, &headers("1.2.3.4"), &config).to_string(), "1.2.3.4");
    }

    #[test]
    fn forwarded_list_takes_the_rightmost_untrusted_entry() {
        let config = config_with(&["10.0.0.1", "10.0.0.2"]);
        let proxy: SocketAddr = "10.0.0.1:1"
            .parse()
            .unwrap_or_else(|_| SocketAddr::from(([0, 0, 0, 0], 0)));
        let ip = client_ip(proxy, &headers("6.6.6.6, 1.2.3.4, 10.0.0.2"), &config);
        assert_eq!(ip.to_string(), "1.2.3.4");
        assert_eq!(client_ip(proxy, &headers("garbage"), &config).to_string(), "10.0.0.1");
    }

    #[test]
    fn mapped_ipv4_peers_are_their_ipv4_client() {
        let limiter = limiter(1, 5, 100);
        let t0 = Instant::now();
        let mapped = |last: u8| IpAddr::V6(std::net::Ipv4Addr::new(198, 51, 100, last).to_ipv6_mapped());
        assert!(allowed(&limiter.check(mapped(1), t0)));
        assert!(
            allowed(&limiter.check(mapped(2), t0)),
            "two IPv4 clients behind a dual-stack listener do not share one bucket"
        );
        assert!(!allowed(&limiter.check(IpAddr::from([198, 51, 100, 1]), t0)));
        let failures = Limiter::new(Limits {
            failures_per_ip: 1,
            ..Limits::default()
        });
        failures.record_failure(mapped(3), t0);
        assert!(!allowed(&failures.check_failures(IpAddr::from([198, 51, 100, 3]), t0)));
        assert!(allowed(&failures.check_failures(mapped(4), t0)));

        let config = config_with(&["10.0.0.1"]);
        let proxy = SocketAddr::new(IpAddr::V6(std::net::Ipv4Addr::new(10, 0, 0, 1).to_ipv6_mapped()), 1);
        assert_eq!(client_ip(proxy, &headers("1.2.3.4"), &config).to_string(), "1.2.3.4");
        let stranger = SocketAddr::new(mapped(9), 1);
        assert_eq!(
            client_ip(stranger, &headers("1.2.3.4"), &config).to_string(),
            "198.51.100.9"
        );
    }

    #[test]
    fn the_ignored_header_warning_is_said_once_per_period() {
        let last = Mutex::new(None);
        let t0 = Instant::now();
        assert!(due(&last, t0, IGNORED_HEADER_EVERY));
        assert!(!due(&last, t0 + Duration::from_mins(9), IGNORED_HEADER_EVERY));
        assert!(due(&last, t0 + IGNORED_HEADER_EVERY, IGNORED_HEADER_EVERY));
    }

    #[test]
    fn failures_block_only_their_own_address_and_expire() {
        let limiter = Limiter::new(Limits {
            failures_per_ip: 2,
            failure_window_secs: 60,
            ..Limits::default()
        });
        let (a, b) = (IpAddr::from([192, 0, 2, 1]), IpAddr::from([192, 0, 2, 2]));
        let now = Instant::now();
        assert!(matches!(limiter.check_failures(a, now), Verdict::Allowed));
        limiter.record_failure(a, now);
        assert!(matches!(limiter.check_failures(a, now), Verdict::Allowed));
        limiter.record_failure(a, now);
        assert!(denied_wait(&limiter.check_failures(a, now)).is_some());
        assert!(matches!(limiter.check_failures(b, now), Verdict::Allowed));
        assert!(matches!(
            limiter.check_failures(a, now + Duration::from_secs(61)),
            Verdict::Allowed
        ));
    }
}
