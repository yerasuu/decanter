// SPDX-License-Identifier: GPL-3.0-or-later
//! The frame both ends write: a big-endian `u16` header length, a protobuf header, then
//! `header.size` payload bytes (comet `utils.rs:27-45`; docs/comet-internals-gap-analysis.md 1.1).

use core::time::Duration;
use std::io;

use tokio::io::{AsyncRead, AsyncReadExt as _, AsyncWrite, AsyncWriteExt as _};
use tokio::time::timeout;

use crate::header::ResponseHeader;
use crate::proto::pb::Header;

/// A header is two to four varints plus, in a response, a short description.
pub const MAX_HEADER_BYTES: u16 = 256;
/// The largest request the service reads. The biggest legitimate ones (a sign-in, a stat update)
/// are well under a kilobyte; Irori's 16 megabytes would let one connection hold that much per frame.
pub const MAX_PAYLOAD_BYTES: u32 = 64 * 1024;
/// How long the rest of a frame may take once its first byte arrived; an idle connection between
/// frames is not limited by it.
pub const PARTIAL_FRAME_TIMEOUT: Duration = Duration::from_secs(5);

/// A header type the codec frames: the generated `Header` for requests, which skips the response
/// extensions, and [`ResponseHeader`] for answers.
pub trait FrameHeader: prost::Message + Default {
    fn payload_size(&self) -> u32;
    fn set_payload_size(&mut self, size: u32);
}

impl FrameHeader for Header {
    fn payload_size(&self) -> u32 {
        self.size.unwrap_or_default()
    }

    fn set_payload_size(&mut self, size: u32) {
        self.size = Some(size);
    }
}

impl FrameHeader for ResponseHeader {
    fn payload_size(&self) -> u32 {
        self.size.unwrap_or_default()
    }

    fn set_payload_size(&mut self, size: u32) {
        self.size = Some(size);
    }
}

#[derive(Debug)]
pub struct Frame<H> {
    pub header: H,
    pub payload: Vec<u8>,
}

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum FrameError {
    #[error("connection: {0}")]
    Io(#[from] io::Error),
    #[error("the rest of a frame did not arrive within {} s", PARTIAL_FRAME_TIMEOUT.as_secs())]
    Timeout,
    #[error("a frame header of {0} bytes is outside 1..={MAX_HEADER_BYTES}")]
    HeaderLength(usize),
    #[error("frame header: {0}")]
    Header(#[from] prost::DecodeError),
    #[error("a payload of {size} bytes is over the {cap}-byte cap")]
    PayloadTooLarge { size: usize, cap: u32 },
}

/// Reads one frame, or `None` when the peer closed the connection between frames. Every length is
/// checked against its cap before anything is allocated for it.
///
/// # Errors
///
/// [`FrameError`] for a closed connection mid-frame, a frame slower than
/// [`PARTIAL_FRAME_TIMEOUT`], a header outside 1..=[`MAX_HEADER_BYTES`], a header that does not
/// decode, or a payload over `max_payload`. The connection is unusable afterwards: the reader is
/// no longer at a frame boundary.
pub async fn read_frame<H, R>(reader: &mut R, max_payload: u32) -> Result<Option<Frame<H>>, FrameError>
where
    H: FrameHeader,
    R: AsyncRead + Unpin,
{
    let high = match reader.read_u8().await {
        Ok(byte) => byte,
        Err(err) if err.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(err) => return Err(err.into()),
    };
    match timeout(PARTIAL_FRAME_TIMEOUT, read_rest(reader, high, max_payload)).await {
        Ok(frame) => frame.map(Some),
        Err(_elapsed) => Err(FrameError::Timeout),
    }
}

async fn read_rest<H, R>(reader: &mut R, high: u8, max_payload: u32) -> Result<Frame<H>, FrameError>
where
    H: FrameHeader,
    R: AsyncRead + Unpin,
{
    let low = reader.read_u8().await?;
    #[expect(
        clippy::big_endian_bytes,
        reason = "the frame's length prefix is big-endian on the wire"
    )]
    let header_len = u16::from_be_bytes([high, low]);
    if header_len == 0 || header_len > MAX_HEADER_BYTES {
        return Err(FrameError::HeaderLength(usize::from(header_len)));
    }
    let mut header_bytes = vec![0_u8; usize::from(header_len)];
    reader.read_exact(&mut header_bytes).await?;
    let header = H::decode(header_bytes.as_slice())?;
    let size = header.payload_size();
    let too_large = || FrameError::PayloadTooLarge {
        size: usize::try_from(size).unwrap_or(usize::MAX),
        cap: max_payload,
    };
    if size > max_payload {
        return Err(too_large());
    }
    let mut payload = vec![0_u8; usize::try_from(size).map_err(|_overflow| too_large())?];
    reader.read_exact(&mut payload).await?;
    Ok(Frame { header, payload })
}

/// Writes one frame with `header.size` set from `payload`, as a single buffer.
///
/// # Errors
///
/// [`FrameError::HeaderLength`] if the encoded header is over [`MAX_HEADER_BYTES`] (a long
/// description), [`FrameError::PayloadTooLarge`] for a payload over `u32::MAX`,
/// [`FrameError::Timeout`] when the peer stops reading for [`PARTIAL_FRAME_TIMEOUT`], and
/// [`FrameError::Io`] for a closed connection.
pub async fn write_frame<H, W>(writer: &mut W, mut header: H, payload: &[u8]) -> Result<(), FrameError>
where
    H: FrameHeader,
    W: AsyncWrite + Unpin,
{
    let size = u32::try_from(payload.len()).map_err(|_overflow| FrameError::PayloadTooLarge {
        size: payload.len(),
        cap: u32::MAX,
    })?;
    header.set_payload_size(size);
    let header_len = header.encoded_len();
    let header_prefix = u16::try_from(header_len)
        .ok()
        .filter(|len| (1..=MAX_HEADER_BYTES).contains(len))
        .ok_or(FrameError::HeaderLength(header_len))?;
    let mut frame = Vec::with_capacity(2_usize.saturating_add(header_len).saturating_add(payload.len()));
    #[expect(
        clippy::big_endian_bytes,
        reason = "the frame's length prefix is big-endian on the wire"
    )]
    frame.extend_from_slice(&header_prefix.to_be_bytes());
    header
        .encode(&mut frame)
        .map_err(|err| io::Error::other(err.to_string()))?;
    frame.extend_from_slice(payload);
    match timeout(PARTIAL_FRAME_TIMEOUT, async {
        writer.write_all(&frame).await?;
        writer.flush().await
    })
    .await
    {
        Ok(written) => written.map_err(FrameError::from),
        Err(_elapsed) => Err(FrameError::Timeout),
    }
}
