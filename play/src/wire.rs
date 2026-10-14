//! Length-prefixed records on a typed unidirectional stream.
//!
//! Each stream starts with a 4-byte kind. Every record after that is
//! `u32le` length, then that many payload bytes.

use etherquic::{note_app, DIR_RECV, DIR_SEND};
use quinn::{RecvStream, SendStream};

use crate::Error;

pub const VIDEO_MAGIC: &[u8; 4] = b"EVID";
pub const AUDIO_MAGIC: &[u8; 4] = b"EAUD";
pub const INPUT_MAGIC: &[u8; 4] = b"EINP";

/// One JPEG, one PCM chunk, or one EQIN document. Larger than a desktop
/// frame at the default width is rejected.
const MAX_RECORD: usize = 12 * 1024 * 1024;

pub async fn write_magic(send: &mut SendStream, magic: &[u8; 4]) -> Result<(), Error> {
    send.write_all(magic)
        .await
        .map_err(|error| Error::Wire(error.to_string()))?;
    note_app(DIR_SEND, magic, &[]);
    Ok(())
}

pub async fn write_record(
    send: &mut SendStream,
    magic: &[u8; 4],
    payload: &[u8],
) -> Result<(), Error> {
    let len =
        u32::try_from(payload.len()).map_err(|_| Error::Wire("record is too large".into()))?;
    send.write_all(&len.to_le_bytes())
        .await
        .map_err(|error| Error::Wire(error.to_string()))?;
    send.write_all(payload)
        .await
        .map_err(|error| Error::Wire(error.to_string()))?;
    note_app(DIR_SEND, magic, payload);
    Ok(())
}

pub async fn read_magic(recv: &mut RecvStream) -> Result<[u8; 4], Error> {
    let mut magic = [0u8; 4];
    if !read_exact(recv, &mut magic).await? {
        return Err(Error::Closed);
    }
    note_app(DIR_RECV, &magic, &[]);
    Ok(magic)
}

/// `Ok(None)` is a clean end of stream before the next record.
pub async fn read_record(recv: &mut RecvStream, magic: &[u8; 4]) -> Result<Option<Vec<u8>>, Error> {
    let mut len_buf = [0u8; 4];
    if !read_exact(recv, &mut len_buf).await? {
        return Ok(None);
    }
    let len = u32::from_le_bytes(len_buf) as usize;
    if len > MAX_RECORD {
        return Err(Error::Wire(format!("record is {len} bytes")));
    }
    let mut payload = vec![0u8; len];
    if !read_exact(recv, &mut payload).await? {
        return Err(Error::Wire("truncated record".into()));
    }
    note_app(DIR_RECV, magic, &payload);
    Ok(Some(payload))
}

/// False when the stream ends before the first byte of `buf`.
async fn read_exact(recv: &mut RecvStream, buf: &mut [u8]) -> Result<bool, Error> {
    let mut filled = 0;
    while filled < buf.len() {
        match recv
            .read(&mut buf[filled..])
            .await
            .map_err(|error| Error::Wire(error.to_string()))?
        {
            Some(0) | None if filled == 0 => return Ok(false),
            Some(0) | None => return Err(Error::Wire("truncated record".into())),
            Some(n) => filled += n,
        }
    }
    Ok(true)
}
