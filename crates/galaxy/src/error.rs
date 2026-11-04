// SPDX-License-Identifier: GPL-3.0-or-later
use core::net::SocketAddr;
use std::io;

use crate::source::ClientId;

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
    /// A running game already holds this client id: two profiles running one game at once, which
    /// the protocol cannot tell apart (plan 1.5).
    #[error("client {0} is already registered by a running game")]
    AlreadyRegistered(ClientId),
    #[error("user id {0} does not fit the 56 bits a Galaxy user id leaves for it")]
    UserIdTooLarge(u64),
}
