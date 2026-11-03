// SPDX-License-Identifier: GPL-3.0-or-later
use core::net::SocketAddr;
use std::io;

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// Real GOG Galaxy, another decanter or some other program holds the port; it is never taken
    /// over (plan 1.4).
    #[error("{0} is already in use by another program")]
    PortInUse(SocketAddr),
    #[error("listening on {addr}: {source}")]
    Bind { addr: SocketAddr, source: io::Error },
    #[error("the listener ended up on {0}, which is not loopback")]
    NotLoopback(SocketAddr),
    #[error("user id {0} does not fit the 56 bits a Galaxy user id leaves for it")]
    UserIdTooLarge(u64),
}
