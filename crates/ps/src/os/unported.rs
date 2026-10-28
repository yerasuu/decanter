//! A system no port is fetched, built or installed on: `PsBuilds` and `PsDownloads` are unavailable
//! there ([`decanter_core::platform::support`]), and every operation says so.

#![expect(
    clippy::unused_async,
    reason = "the same signatures as the systems whose parts await, so the facade calls each alike"
)]

use std::io;
use std::path::PathBuf;

use decanter_core::capabilities::{Feature, Support};
use decanter_core::platform;
use decanter_core::runner::{Installed, Pin, Progress, Status};

use crate::paths::Root;
use crate::port::Port;
use crate::runner::Error;

fn unsupported(port: Port) -> io::Error {
    io::Error::new(io::ErrorKind::Unsupported, Error::Unsupported(port.name()))
}

pub const fn support(_: Port) -> Support {
    platform::support(Feature::PsBuilds)
}

pub async fn status(_: &Root, _: Port) -> Status {
    Status::Unsupported
}

pub async fn confirm(_: &Root, port: Port, _: &Pin) -> io::Result<()> {
    Err(unsupported(port))
}

pub async fn prepare(
    _: &Root,
    port: Port,
    _: &Pin,
    _: &mut dyn FnMut(Progress),
    _: &mut dyn FnMut(String),
) -> Result<Installed, Error> {
    Err(Error::Unsupported(port.name()))
}

pub async fn update(
    _: &Root,
    port: Port,
    _: &mut dyn FnMut(Progress),
    _: &mut dyn FnMut(String),
) -> Result<Installed, Error> {
    Err(Error::Unsupported(port.name()))
}

pub async fn remove(_: &Root, port: Port, _: &Pin) -> io::Result<()> {
    Err(unsupported(port))
}

pub const fn log_path(_: &Root, _: Port, _: &Pin) -> Option<PathBuf> {
    None
}
