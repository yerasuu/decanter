// SPDX-License-Identifier: GPL-3.0-or-later
use crate::proto::pb::response::Status;

/// `gog.protocols.pb.Header` with the `Response` extensions (100-103) as plain fields.
///
/// prost generates no proto2 extensions, so its `Header` would drop them. On the wire an extension
/// is an ordinary field of the message it extends, so the generated `Header` decodes this as
/// fields 1-4 and skips the rest.
#[derive(Clone, PartialEq, Eq, prost::Message)]
pub struct ResponseHeader {
    #[prost(uint32, required, tag = "1")]
    pub sort: u32,
    #[prost(uint32, required, tag = "2")]
    pub r#type: u32,
    #[prost(uint32, optional, tag = "3")]
    pub size: Option<u32>,
    #[prost(uint32, optional, tag = "4")]
    pub oseq: Option<u32>,
    /// The `oseq` of the request this answers.
    #[prost(uint32, optional, tag = "100")]
    pub rseq: Option<u32>,
    #[prost(enumeration = "Status", optional, tag = "101")]
    pub code: Option<i32>,
    #[prost(string, optional, tag = "102")]
    pub description: Option<String>,
    /// Seconds.
    #[prost(uint32, optional, tag = "103")]
    pub retry_after: Option<u32>,
}
