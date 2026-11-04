// SPDX-License-Identifier: GPL-3.0-or-later
//! What one client may hold or ask for (plan 2.4), and the count of every refusal, so that a
//! hostile or broken client costs bounded memory and time and never goes unnoticed.

use core::fmt::{self, Write as _};
use core::sync::atomic::{AtomicU64, Ordering};
use core::time::Duration;

use tokio::time::Instant;

/// A few games at once, each with a couple of SDK connections.
pub(crate) const MAX_CONNECTIONS: usize = 16;
/// Connections not yet signed in. The plan's one would refuse a game that opens its connections
/// in parallel; four still bounds what a client that never signs in can hold.
pub(crate) const MAX_UNAUTHENTICATED: usize = 4;
pub(crate) const MAX_CONNECTIONS_PER_GAME: usize = 4;
/// From the accept to the first accepted `AUTH_INFO`, procfs check included.
pub(crate) const SIGN_IN_DEADLINE: Duration = Duration::from_secs(10);
pub(crate) const REQUESTS_PER_SECOND: u32 = 200;
pub(crate) const REQUEST_BURST: u32 = 400;
/// Unlocks, clears and stat updates of one game, across its connections. The burst lets a game
/// store a run's worth of unlocks at once, since whether the SDK retries a refused one is unknown.
pub(crate) const WRITES_PER_SECOND: u32 = 20;
pub(crate) const WRITE_BURST: u32 = 100;
/// Keeps a response header, whose description may quote a store's reason, under the codec's cap.
pub(crate) const MAX_DESCRIPTION_BYTES: usize = 160;

/// A token bucket over integer time, so that its arithmetic cannot overflow or round into a
/// burst larger than configured.
pub(crate) struct TokenBucket {
    interval: Duration,
    burst: u32,
    available: u32,
    refilled_at: Instant,
}

impl TokenBucket {
    pub(crate) fn new(per_second: u32, burst: u32, now: Instant) -> Self {
        Self {
            interval: Duration::from_secs(1)
                .checked_div(per_second)
                .unwrap_or(Duration::from_secs(1)),
            burst,
            available: burst,
            refilled_at: now,
        }
    }

    /// Takes one token, or tells how long until the next one.
    pub(crate) fn take(&mut self, now: Instant) -> Result<(), Duration> {
        if self.available >= self.burst {
            self.refilled_at = now;
        } else {
            let elapsed = now.saturating_duration_since(self.refilled_at);
            let earned = elapsed
                .as_nanos()
                .checked_div(self.interval.as_nanos())
                .and_then(|earned| u32::try_from(earned).ok())
                .unwrap_or(u32::MAX);
            if earned > 0 {
                self.available = self.available.saturating_add(earned).min(self.burst);
                self.refilled_at = if self.available >= self.burst {
                    now
                } else {
                    self.interval
                        .checked_mul(earned)
                        .and_then(|spent| self.refilled_at.checked_add(spent))
                        .unwrap_or(now)
                };
            }
        }
        match self.available.checked_sub(1) {
            Some(left) => {
                self.available = left;
                Ok(())
            }
            None => Err(self
                .interval
                .saturating_sub(now.saturating_duration_since(self.refilled_at))),
        }
    }
}

/// `retry_after` is in whole seconds and the SDK should not retry at once.
pub(crate) fn retry_after_secs(wait: Duration) -> u32 {
    let whole = wait.as_secs().saturating_add(u64::from(wait.subsec_nanos() > 0));
    u32::try_from(whole).unwrap_or(u32::MAX).max(1)
}

#[derive(Clone, Copy, Debug)]
pub(crate) enum Refusal {
    NotLoopback,
    OtherUser,
    OwnerUnknown,
    TooManyConnections,
    TooManyUnauthenticated,
    SignInTimeout,
    BadFrame,
    RateLimited,
    MalformedClientId,
    GalaxyClientId,
    UnregisteredClient,
    GameConnectionsFull,
    WritesRateLimited,
    UnknownRequest,
}

impl Refusal {
    const ALL: [Self; 14] = [
        Self::NotLoopback,
        Self::OtherUser,
        Self::OwnerUnknown,
        Self::TooManyConnections,
        Self::TooManyUnauthenticated,
        Self::SignInTimeout,
        Self::BadFrame,
        Self::RateLimited,
        Self::MalformedClientId,
        Self::GalaxyClientId,
        Self::UnregisteredClient,
        Self::GameConnectionsFull,
        Self::WritesRateLimited,
        Self::UnknownRequest,
    ];

    const fn name(self) -> &'static str {
        match self {
            Self::NotLoopback => "not_loopback",
            Self::OtherUser => "other_user",
            Self::OwnerUnknown => "owner_unknown",
            Self::TooManyConnections => "too_many_connections",
            Self::TooManyUnauthenticated => "too_many_unauthenticated",
            Self::SignInTimeout => "sign_in_timeout",
            Self::BadFrame => "bad_frame",
            Self::RateLimited => "rate_limited",
            Self::MalformedClientId => "malformed_client_id",
            Self::GalaxyClientId => "galaxy_client_id",
            Self::UnregisteredClient => "unregistered_client",
            Self::GameConnectionsFull => "game_connections_full",
            Self::WritesRateLimited => "writes_rate_limited",
            Self::UnknownRequest => "unknown_request",
        }
    }
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// Every refusal of one listener's lifetime, by kind.
#[derive(Default)]
pub(crate) struct Refusals {
    counts: [AtomicU64; Refusal::ALL.len()],
}

impl Refusals {
    /// Counts one, and returns the new total when this one should be logged: the first few and
    /// then every power of two, so that a flood stays visible without flooding the log itself.
    pub(crate) fn record(&self, kind: Refusal) -> Option<u64> {
        #[expect(
            clippy::as_conversions,
            reason = "a fieldless enum's discriminant indexes its own counter"
        )]
        let index = kind as usize;
        let total = self
            .counts
            .get(index)
            .map_or(0, |count| count.fetch_add(1, Ordering::Relaxed).saturating_add(1));
        (total <= 8 || total.is_power_of_two()).then_some(total)
    }

    /// `kind=count` for every kind that happened, for the listener's closing line.
    pub(crate) fn summary(&self) -> String {
        let mut text = String::new();
        for (kind, counter) in Refusal::ALL.iter().zip(&self.counts) {
            let count = counter.load(Ordering::Relaxed);
            if count == 0 {
                continue;
            }
            let separator = if text.is_empty() { "" } else { " " };
            if write!(text, "{separator}{kind}={count}").is_err() {
                break;
            }
        }
        text
    }
}
