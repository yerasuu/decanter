// SPDX-License-Identifier: GPL-3.0-or-later
//! Generates the Galaxy messages from `proto/` into `OUT_DIR`, one `.rs` per protobuf package.

#![expect(
    clippy::question_mark_used,
    clippy::implicit_return,
    reason = "a build script fails the build by returning the error; it never runs in the shipped binary"
)]
#![expect(
    clippy::large_stack_frames,
    reason = "protox's descriptor set is built once, at build time"
)]

use core::error::Error;

/// Every package the service decodes; the overlay and peer files of `gog_protocols` are not vendored.
const PROTOS: [&str; 4] = [
    "gog.protocols.pb.proto",
    "galaxy.protocols.communication_service.proto",
    "galaxy.protocols.webbroker_service.proto",
    "galaxy.common.protocols.connection.proto",
];

fn main() -> Result<(), Box<dyn Error>> {
    println!("cargo::rerun-if-changed=proto");
    let descriptors = protox::compile(PROTOS, ["proto"])?;
    prost_build::Config::new().compile_fds(descriptors)?;
    Ok(())
}
