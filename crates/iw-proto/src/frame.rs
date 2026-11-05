//! Length-prefixed framing shared by IPC and peer streams.
//!
//! Each frame is a big-endian `u32` length followed by one protobuf message.
//! Frames larger than [`MAX_FRAME_LEN`] are rejected on both ends, before any
//! payload is buffered, so a misbehaving peer or client cannot make the agent
//! allocate unbounded memory. Streams that only carry small messages, such as
//! pairing, read with a lower limit.

use bytes::{Buf, BufMut, BytesMut};
use prost::Message;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

pub const MAX_FRAME_LEN: usize = 4 * 1024 * 1024;

const HEADER_LEN: usize = 4;

#[derive(Debug, thiserror::Error)]
pub enum FrameError {
    #[error("frame of {len} bytes exceeds the {limit}-byte limit")]
    TooLarge { len: usize, limit: usize },
    #[error("malformed message: {0}")]
    Decode(#[from] prost::DecodeError),
    #[error("stream ended in the middle of a frame")]
    Truncated,
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// Appends `msg` to `dst` as one frame.
pub fn encode<M: Message>(msg: &M, dst: &mut BytesMut) -> Result<(), FrameError> {
    let len = msg.encoded_len();
    if len > MAX_FRAME_LEN {
        return Err(FrameError::TooLarge {
            len,
            limit: MAX_FRAME_LEN,
        });
    }
    dst.reserve(HEADER_LEN + len);
    dst.put_u32(len as u32);
    msg.encode(dst)
        .expect("buffer has room reserved for the message");
    Ok(())
}

/// Removes one complete frame from the front of `src` and decodes it.
/// Returns `Ok(None)` if `src` does not yet hold a complete frame.
pub fn decode<M: Message + Default>(src: &mut BytesMut) -> Result<Option<M>, FrameError> {
    decode_limited(src, MAX_FRAME_LEN)
}

/// Like [`decode`], but rejects frames longer than `limit` bytes.
pub fn decode_limited<M: Message + Default>(
    src: &mut BytesMut,
    limit: usize,
) -> Result<Option<M>, FrameError> {
    if src.len() < HEADER_LEN {
        return Ok(None);
    }
    let len = u32::from_be_bytes([src[0], src[1], src[2], src[3]]) as usize;
    if len > limit {
        return Err(FrameError::TooLarge { len, limit });
    }
    if src.len() < HEADER_LEN + len {
        src.reserve(HEADER_LEN + len - src.len());
        return Ok(None);
    }
    src.advance(HEADER_LEN);
    let body = src.split_to(len);
    Ok(Some(M::decode(body)?))
}

/// Reads the next frame, using `buf` to hold bytes read past it.
/// Returns `Ok(None)` when the stream ends cleanly between frames.
///
/// Cancel-safe: bytes already read stay in `buf` for the next call.
pub async fn read_frame<M, R>(reader: &mut R, buf: &mut BytesMut) -> Result<Option<M>, FrameError>
where
    M: Message + Default,
    R: AsyncRead + Unpin,
{
    read_frame_limited(reader, buf, MAX_FRAME_LEN).await
}

/// Like [`read_frame`], but rejects frames longer than `limit` bytes.
pub async fn read_frame_limited<M, R>(
    reader: &mut R,
    buf: &mut BytesMut,
    limit: usize,
) -> Result<Option<M>, FrameError>
where
    M: Message + Default,
    R: AsyncRead + Unpin,
{
    loop {
        if let Some(msg) = decode_limited(buf, limit)? {
            return Ok(Some(msg));
        }
        if reader.read_buf(buf).await? == 0 {
            return if buf.is_empty() {
                Ok(None)
            } else {
                Err(FrameError::Truncated)
            };
        }
    }
}

pub async fn write_frame<M, W>(writer: &mut W, msg: &M) -> Result<(), FrameError>
where
    M: Message,
    W: AsyncWrite + Unpin,
{
    let mut buf = BytesMut::new();
    encode(msg, &mut buf)?;
    writer.write_all(&buf).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::v1::{ClientMessage, GetStatus, SetSharing, client_message::Command};

    fn get_status(id: u64) -> ClientMessage {
        ClientMessage {
            request_id: id,
            command: Some(Command::GetStatus(GetStatus {})),
        }
    }

    #[test]
    fn round_trips_several_frames() {
        let mut buf = BytesMut::new();
        encode(&get_status(1), &mut buf).unwrap();
        let second = ClientMessage {
            request_id: 2,
            command: Some(Command::SetSharing(SetSharing { enabled: false })),
        };
        encode(&second, &mut buf).unwrap();

        assert_eq!(
            decode::<ClientMessage>(&mut buf).unwrap(),
            Some(get_status(1))
        );
        assert_eq!(decode::<ClientMessage>(&mut buf).unwrap(), Some(second));
        assert_eq!(decode::<ClientMessage>(&mut buf).unwrap(), None);
        assert!(buf.is_empty());
    }

    #[test]
    fn waits_for_a_complete_frame() {
        let mut full = BytesMut::new();
        encode(&get_status(7), &mut full).unwrap();

        let mut buf = BytesMut::new();
        for byte in &full[..full.len() - 1] {
            buf.put_u8(*byte);
            assert_eq!(decode::<ClientMessage>(&mut buf).unwrap(), None);
        }
        buf.put_u8(full[full.len() - 1]);
        assert_eq!(
            decode::<ClientMessage>(&mut buf).unwrap(),
            Some(get_status(7))
        );
    }

    #[test]
    fn rejects_oversized_frames_from_the_header_alone() {
        let mut buf = BytesMut::new();
        buf.put_u32((MAX_FRAME_LEN + 1) as u32);
        assert!(matches!(
            decode::<ClientMessage>(&mut buf),
            Err(FrameError::TooLarge { len, .. }) if len == MAX_FRAME_LEN + 1
        ));
    }

    #[test]
    fn honours_a_lower_limit() {
        let mut buf = BytesMut::new();
        encode(&get_status(1), &mut buf).unwrap();
        let len = buf.len() - HEADER_LEN;
        assert!(matches!(
            decode_limited::<ClientMessage>(&mut buf.clone(), len - 1),
            Err(FrameError::TooLarge { limit, .. }) if limit == len - 1
        ));
        assert_eq!(
            decode_limited::<ClientMessage>(&mut buf, len).unwrap(),
            Some(get_status(1))
        );
    }

    #[test]
    fn reports_malformed_payloads() {
        let mut buf = BytesMut::new();
        buf.put_u32(2);
        buf.put_slice(&[0xff, 0xff]);
        assert!(matches!(
            decode::<ClientMessage>(&mut buf),
            Err(FrameError::Decode(_))
        ));
    }

    #[tokio::test]
    async fn streams_frames_and_detects_truncation() {
        let (mut a, mut b) = tokio::io::duplex(64);
        write_frame(&mut a, &get_status(1)).await.unwrap();
        write_frame(&mut a, &get_status(2)).await.unwrap();
        a.write_all(&[0, 0, 0, 9, 1]).await.unwrap();
        drop(a);

        let mut buf = BytesMut::new();
        assert_eq!(
            read_frame::<ClientMessage, _>(&mut b, &mut buf)
                .await
                .unwrap(),
            Some(get_status(1))
        );
        assert_eq!(
            read_frame::<ClientMessage, _>(&mut b, &mut buf)
                .await
                .unwrap(),
            Some(get_status(2))
        );
        assert!(matches!(
            read_frame::<ClientMessage, _>(&mut b, &mut buf).await,
            Err(FrameError::Truncated)
        ));
    }

    #[tokio::test]
    async fn clean_end_of_stream_is_none() {
        let (a, mut b) = tokio::io::duplex(64);
        drop(a);
        let mut buf = BytesMut::new();
        assert_eq!(
            read_frame::<ClientMessage, _>(&mut b, &mut buf)
                .await
                .unwrap(),
            None
        );
    }
}
