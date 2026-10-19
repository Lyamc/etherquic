//! Length-prefixed records, the same framing as the legacy streams: a
//! 4-byte kind once, then `u32le` length and payload per record.

use etherquic::{note_app, DIR_RECV, DIR_SEND};
use quinn::{RecvStream, SendStream};

const MAX_RECORD: usize = 24 * 1024 * 1024;

fn err(error: impl std::fmt::Display) -> String {
    error.to_string()
}

pub async fn write_magic(send: &mut SendStream, magic: &[u8; 4]) -> Result<(), String> {
    send.write_all(magic).await.map_err(err)?;
    note_app(DIR_SEND, magic, &[]);
    Ok(())
}

pub async fn write_record(
    send: &mut SendStream,
    magic: &[u8; 4],
    payload: &[u8],
) -> Result<(), String> {
    let len = u32::try_from(payload.len()).map_err(|_| "record too large".to_string())?;
    send.write_all(&len.to_le_bytes()).await.map_err(err)?;
    send.write_all(payload).await.map_err(err)?;
    note_app(DIR_SEND, magic, payload);
    Ok(())
}

/// A record whose payload is `kind` then `payload`, without copying.
pub async fn write_typed(
    send: &mut SendStream,
    magic: &[u8; 4],
    kind: u8,
    payload: &[u8],
) -> Result<(), String> {
    let len = u32::try_from(payload.len() + 1).map_err(|_| "record too large".to_string())?;
    send.write_all(&len.to_le_bytes()).await.map_err(err)?;
    send.write_all(&[kind]).await.map_err(err)?;
    send.write_all(payload).await.map_err(err)?;
    note_app(DIR_SEND, magic, payload);
    Ok(())
}

pub async fn read_magic(recv: &mut RecvStream) -> Result<Option<[u8; 4]>, String> {
    let mut magic = [0u8; 4];
    if !read_exact(recv, &mut magic).await? {
        return Ok(None);
    }
    note_app(DIR_RECV, &magic, &[]);
    Ok(Some(magic))
}

pub async fn read_record(
    recv: &mut RecvStream,
    magic: &[u8; 4],
) -> Result<Option<Vec<u8>>, String> {
    let mut len_buf = [0u8; 4];
    if !read_exact(recv, &mut len_buf).await? {
        return Ok(None);
    }
    let len = u32::from_le_bytes(len_buf) as usize;
    if len > MAX_RECORD {
        return Err(format!("record is {len} bytes"));
    }
    let mut payload = vec![0u8; len];
    if !read_exact(recv, &mut payload).await? {
        return Err("truncated record".into());
    }
    note_app(DIR_RECV, magic, &payload);
    Ok(Some(payload))
}

async fn read_exact(recv: &mut RecvStream, buf: &mut [u8]) -> Result<bool, String> {
    let mut filled = 0;
    while filled < buf.len() {
        match recv.read(&mut buf[filled..]).await.map_err(err)? {
            Some(0) | None if filled == 0 => return Ok(false),
            Some(0) | None => return Err("truncated record".into()),
            Some(n) => filled += n,
        }
    }
    Ok(true)
}
