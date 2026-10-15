//! One play session on the QUIC connection the service already accepted.
//!
//! The host opens the picture and audio streams. The player opens input.
//! Same magics, priorities, and record layout as `etherquic-play`.

#![forbid(unsafe_code)]

use etherquic::{PRIORITY_AUDIO, PRIORITY_VIDEO};

/// Above audio and video so a timing report is not queued behind a picture.
const PRIORITY_TRACE: i32 = 30;
const ACK_MAGIC: &[u8; 4] = b"EACK";
use etherquic_input::{decode, Event};
use quinn::{Connection, RecvStream, SendStream};
use tokio::sync::{mpsc, watch};

use crate::proto::{self, AudioChunk, AUDIO_MAGIC, INPUT_MAGIC, VIDEO_MAGIC};

/// Send desktop and audio until the player leaves or both senders close.
pub async fn serve(
    conn: Connection,
    mut video: watch::Receiver<Vec<u8>>,
    mut audio: mpsc::Receiver<AudioChunk>,
    input: mpsc::Sender<Vec<Event>>,
    acks: mpsc::Receiver<Vec<u8>>,
) -> Result<(), String> {
    // Picture and audio open before input is read, so a confirmation stream
    // cannot be accepted in their place.
    let mut video_send = open_uni(&conn, PRIORITY_VIDEO, VIDEO_MAGIC).await?;
    let mut audio_send = open_uni(&conn, PRIORITY_AUDIO, AUDIO_MAGIC).await?;
    let input_conn = conn.clone();
    let input_task = tokio::spawn(async move { read_input(input_conn, input).await });
    let ack_conn = conn.clone();
    let ack_task = tokio::spawn(async move { write_acks(ack_conn, acks).await });
    let mut video_open = true;
    let mut audio_open = true;
    let closed = conn.closed();
    tokio::pin!(closed);
    loop {
        if !video_open && !audio_open {
            break;
        }
        tokio::select! {
            _ = &mut closed => break,
            msg = audio.recv(), if audio_open => {
                match msg {
                    Some(chunk) => {
                        let payload = chunk.encode()?;
                        write_record(&mut audio_send, &payload).await?;
                    }
                    None => {
                        let _ = finish(&mut audio_send).await;
                        audio_open = false;
                    }
                }
            }
            changed = video.changed(), if video_open => {
                if changed.is_err() {
                    let _ = finish(&mut video_send).await;
                    video_open = false;
                    continue;
                }
                let jpeg = video.borrow().clone();
                if jpeg.is_empty() {
                    continue;
                }
                if jpeg.len() > proto::MAX_PAYLOAD {
                    continue;
                }
                write_record(&mut video_send, &jpeg).await?;
            }
        }
    }
    ack_task.abort();
    input_task.abort();
    Ok(())
}

async fn write_acks(conn: Connection, mut acks: mpsc::Receiver<Vec<u8>>) -> Result<(), String> {
    let mut send = None;
    while let Some(payload) = acks.recv().await {
        if payload.len() != 15 {
            continue;
        }
        if send.is_none() {
            send = Some(open_uni(&conn, PRIORITY_TRACE, ACK_MAGIC).await?);
        }
        if let Some(stream) = send.as_mut() {
            write_record(stream, &payload).await?;
        }
    }
    Ok(())
}

async fn open_uni(conn: &Connection, priority: i32, magic: &[u8; 4]) -> Result<SendStream, String> {
    let send = conn
        .open_uni()
        .await
        .map_err(|error| error.to_string())?;
    send.set_priority(priority)
        .map_err(|error| error.to_string())?;
    let mut send = send;
    send.write_all(magic)
        .await
        .map_err(|error| error.to_string())?;
    Ok(send)
}

async fn write_record(send: &mut SendStream, payload: &[u8]) -> Result<(), String> {
    let len = u32::try_from(payload.len()).map_err(|_| "record is too large".to_string())?;
    send.write_all(&len.to_le_bytes())
        .await
        .map_err(|error| error.to_string())?;
    send.write_all(payload)
        .await
        .map_err(|error| error.to_string())
}

async fn finish(send: &mut SendStream) -> Result<(), String> {
    send.finish().map_err(|error| error.to_string())?;
    send.stopped()
        .await
        .map(|_| ())
        .map_err(|error| error.to_string())
}

async fn read_input(conn: Connection, input: mpsc::Sender<Vec<Event>>) -> Result<(), String> {
    let mut recv = conn
        .accept_uni()
        .await
        .map_err(|error| error.to_string())?;
    let mut magic = [0u8; 4];
    if !read_exact(&mut recv, &mut magic).await? {
        return Ok(());
    }
    if &magic != INPUT_MAGIC {
        return Err(format!(
            "expected input stream, got {}",
            String::from_utf8_lossy(&magic)
        ));
    }
    loop {
        let mut len_buf = [0u8; 4];
        if !read_exact(&mut recv, &mut len_buf).await? {
            return Ok(());
        }
        let len = u32::from_le_bytes(len_buf) as usize;
        if len > proto::MAX_PAYLOAD {
            return Err(format!("record is {len} bytes"));
        }
        let mut payload = vec![0u8; len];
        if !read_exact(&mut recv, &mut payload).await? {
            return Err("truncated record".into());
        }
        let events = decode(&payload).map_err(|error| error.to_string())?;
        if input.send(events).await.is_err() {
            return Ok(());
        }
    }
}

/// False when the stream ends before the first byte.
async fn read_exact(recv: &mut RecvStream, buf: &mut [u8]) -> Result<bool, String> {
    let mut filled = 0;
    while filled < buf.len() {
        match recv
            .read(&mut buf[filled..])
            .await
            .map_err(|error| error.to_string())?
        {
            Some(0) | None if filled == 0 => return Ok(false),
            Some(0) | None => return Err("truncated record".into()),
            Some(n) => filled += n,
        }
    }
    Ok(true)
}
