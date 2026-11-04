// SPDX-License-Identifier: GPL-3.0-or-later
//! The same-user check (plan 1.5, requirement 3): every process of the machine can dial the
//! loopback port, so a connection is served only when the socket that dialled it belongs to the
//! user decanter runs as. Linux reads both owners from procfs; other systems have no method yet,
//! so their peers are never known.
//!
//! Kept free of the rest of the crate so that `examples/fuzz_probe.rs` can run the table parser
//! over crafted procfs content.

use core::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use core::str;
#[cfg(target_os = "linux")]
use std::fs;
use std::io;

/// Fail closed: a peer whose owner cannot be read is refused like one of another user. Flipping
/// this lets every local program that knows a registered client id sign in wherever procfs is
/// hidden or unmapped (a sandboxed decanter, a system without `/proc/net`), so it is a build-time
/// choice for such a place, never a setting.
pub(crate) const ACCEPT_PEERS_OF_UNKNOWN_OWNER: bool = false;

/// `TCP_TIME_WAIT` in `include/net/tcp_states.h`. Such entries are kernel leftovers listed with
/// uid 0, so they never name the peer's owner.
const TIME_WAIT: u8 = 0x06;
/// What the kernel shows for a uid the reader's user namespace does not map, unless the sysctl
/// says otherwise.
const DEFAULT_OVERFLOW_UID: u32 = 0xFFFE;

#[derive(Debug)]
pub(crate) enum Verdict {
    SameUser,
    OtherUser { peer_uid: u32, own_uid: u32 },
    Unknown(OwnerUnknown),
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum OwnerUnknown {
    #[cfg(not(target_os = "linux"))]
    #[error("this system has no method to read a socket's owner")]
    Unsupported,
    #[error("reading {path}: {source}")]
    Unreadable { path: &'static str, source: io::Error },
    #[error("{0} does not say whose it is")]
    Malformed(&'static str),
    /// The peer closed before the check (its socket is gone or in `TIME_WAIT`), or it lives in a
    /// network namespace whose table this process does not see.
    #[error("the peer's socket is not listed")]
    NotListed,
    #[error("the peer's socket is listed under {first} and {second}")]
    Ambiguous { first: u32, second: u32 },
    /// The owner exists, but outside the user namespace decanter runs in (a sandboxed decanter).
    #[error("the peer's owner is not mapped into decanter's user namespace (uid {0})")]
    Unmapped(u32),
}

/// Blocks on procfs reads: call it on the blocking pool. `local` is the service's end of the
/// connection.
#[cfg(target_os = "linux")]
pub(crate) fn verdict(peer: SocketAddr, local: SocketAddr) -> Verdict {
    let own_uid = match own_uid() {
        Ok(uid) => uid,
        Err(err) => return Verdict::Unknown(err),
    };
    match peer_uid(peer, local) {
        Ok(peer_uid) if peer_uid == own_uid => Verdict::SameUser,
        Ok(peer_uid) if peer_uid == overflow_uid() => Verdict::Unknown(OwnerUnknown::Unmapped(peer_uid)),
        Ok(peer_uid) => Verdict::OtherUser { peer_uid, own_uid },
        Err(err) => Verdict::Unknown(err),
    }
}

#[cfg(not(target_os = "linux"))]
pub(crate) const fn verdict(_peer: SocketAddr, _local: SocketAddr) -> Verdict {
    Verdict::Unknown(OwnerUnknown::Unsupported)
}

/// The filesystem uid (the fourth `Uid:` field), because the kernel records a socket's owner from
/// the creating process's filesystem uid.
#[cfg(target_os = "linux")]
fn own_uid() -> Result<u32, OwnerUnknown> {
    const PATH: &str = "/proc/self/status";
    let status = fs::read_to_string(PATH).map_err(|source| OwnerUnknown::Unreadable { path: PATH, source })?;
    status
        .lines()
        .find_map(|line| line.strip_prefix("Uid:"))
        .and_then(|ids| ids.split_ascii_whitespace().nth(3))
        .and_then(|uid| uid.parse().ok())
        .ok_or(OwnerUnknown::Malformed(PATH))
}

#[cfg(target_os = "linux")]
fn overflow_uid() -> u32 {
    fs::read_to_string("/proc/sys/kernel/overflowuid")
        .ok()
        .and_then(|text| text.trim().parse().ok())
        .unwrap_or(DEFAULT_OVERFLOW_UID)
}

/// A client on a dual-stack socket dialling `127.0.0.1` is listed in `tcp6` with v4-mapped
/// addresses, so both tables are searched.
#[cfg(target_os = "linux")]
fn peer_uid(peer: SocketAddr, local: SocketAddr) -> Result<u32, OwnerUnknown> {
    const TCP: &str = "/proc/net/tcp";
    const TCP6: &str = "/proc/net/tcp6";
    let table = fs::read_to_string(TCP).map_err(|source| OwnerUnknown::Unreadable { path: TCP, source })?;
    if let Some(uid) = find_owner(&table, peer, local)? {
        return Ok(uid);
    }
    let table6 = match fs::read_to_string(TCP6) {
        Ok(table6) => table6,
        // A kernel without IPv6 has no tcp6 table, and then no peer to find in it.
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Err(OwnerUnknown::NotListed),
        Err(source) => return Err(OwnerUnknown::Unreadable { path: TCP6, source }),
    };
    find_owner(&table6, peer, local)?.ok_or(OwnerUnknown::NotListed)
}

/// The uid of the socket in `table` (the text of `/proc/net/tcp` or `tcp6`) whose local end is
/// `peer` and whose remote end is `local`: the client's own socket. `None` when no live entry
/// matches; lines that do not parse are skipped, since they cannot be the peer's.
///
/// # Errors
///
/// [`OwnerUnknown::Ambiguous`] when two live entries match with different owners.
pub(crate) fn find_owner(table: &str, peer: SocketAddr, local: SocketAddr) -> Result<Option<u32>, OwnerUnknown> {
    let (client, service) = (canonical(peer), canonical(local));
    let mut found = None;
    for entry in table.lines().filter_map(entry) {
        if entry.local != client || entry.remote != service || entry.state == TIME_WAIT {
            continue;
        }
        match found {
            Some(first) if first != entry.uid => {
                return Err(OwnerUnknown::Ambiguous {
                    first,
                    second: entry.uid,
                });
            }
            _ => found = Some(entry.uid),
        }
    }
    Ok(found)
}

struct Entry {
    local: SocketAddr,
    remote: SocketAddr,
    state: u8,
    uid: u32,
}

/// `sl local_address rem_address st tx_queue:rx_queue tr:tm->when retrnsmt uid ...`, as
/// `tcp4_seq_show` and `tcp6_seq_show` print it.
fn entry(line: &str) -> Option<Entry> {
    let mut fields = line.split_ascii_whitespace();
    let _slot = fields.next()?;
    let local = socket_addr(fields.next()?)?;
    let remote = socket_addr(fields.next()?)?;
    let state = u8::from_str_radix(fields.next()?, 16).ok()?;
    let uid = fields.nth(3)?.parse().ok()?;
    Some(Entry {
        local,
        remote,
        state,
        uid,
    })
}

/// `ADDRESS:PORT` in hex: the address as 32-bit words in host byte order, the port as a number.
fn socket_addr(field: &str) -> Option<SocketAddr> {
    let (address, port_hex) = field.split_once(':')?;
    let port = u16::from_str_radix(port_hex, 16).ok()?;
    let ip = match address.len() {
        8 => IpAddr::V4(Ipv4Addr::from(host_order_word(address)?)),
        32 => {
            let mut octets = [0_u8; 16];
            let (out_words, _) = octets.as_chunks_mut::<4>();
            let (hex_words, _) = address.as_bytes().as_chunks::<8>();
            for (out, hex) in out_words.iter_mut().zip(hex_words) {
                *out = host_order_word(str::from_utf8(hex).ok()?)?;
            }
            IpAddr::V6(Ipv6Addr::from(octets))
        }
        _ => return None,
    };
    Some(canonical(SocketAddr::new(ip, port)))
}

/// Each address word is printed as a number in host byte order, so its bytes come back with the
/// host's order too.
fn host_order_word(hex: &str) -> Option<[u8; 4]> {
    if !hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }
    u32::from_str_radix(hex, 16).ok().map(u32::to_ne_bytes)
}

const fn canonical(addr: SocketAddr) -> SocketAddr {
    SocketAddr::new(addr.ip().to_canonical(), addr.port())
}
