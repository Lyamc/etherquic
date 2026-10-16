//! One play session on the QUIC connection the service already accepted.
//!
//! The host opens the picture and audio streams. The player opens input.
//! Same magics, priorities, and record layout as `etherquic-play`.

#![forbid(unsafe_code)]

/// Above audio and video so a timing report is not queued behind a picture.
const PRIORITY_TRACE: i32 = 30;
const ACK_MAGIC: &[u8; 4] = b"EACK";
use etherquic_input::{decode, Event};
use etherquic_media::host::{self, FrameSlot};
use quinn::{Connection, RecvStream, SendStream};
use tokio::sync::mpsc;

use crate::proto::{self, AudioChunk, INPUT_MAGIC};

/// Send desktop and audio until the player leaves.
///
/// A player on `etherquic/2` sends a codec offer first and gets the first
/// codec both ends run; an older player gets MJPEG and PCM on `EVID`/`EAUD`.
/// Either way the service encodes the helper's raw frames here.
pub async fn serve(
    conn: Connection,
    video: FrameSlot,
    mut audio: mpsc::Receiver<AudioChunk>,
    input: mpsc::Sender<Vec<Event>>,
    acks: mpsc::Receiver<Vec<u8>>,
    log: etherquic_media::Log,
    cursor: tokio::sync::watch::Receiver<Option<Vec<u8>>>,
) -> Result<(), String> {
    let (negotiated, stray) = host::accept_offer(&conn).await?;
    if let Some((_, magic)) = stray {
        return Err(format!(
            "expected a codec offer, got {}",
            String::from_utf8_lossy(&magic)
        ));
    }
    log(if negotiated.is_v2() {
        "player negotiates codecs"
    } else {
        "player does not negotiate; MJPEG and PCM"
    });
    // Picture and audio open before input is read, so a confirmation stream
    // cannot be accepted in their place.
    let streams = host::open_streams(&conn, negotiated.is_v2()).await?;
    let input_conn = conn.clone();
    let input_task = tokio::spawn(async move { read_input(input_conn, input).await });
    let ack_conn = conn.clone();
    let ack_task = tokio::spawn(async move { write_acks(ack_conn, acks).await });
    let (pcm_tx, pcm_rx) = mpsc::channel(8);
    let forward = tokio::spawn(async move {
        while let Some(chunk) = audio.recv().await {
            let pcm = etherquic_media::Pcm {
                sample_rate: chunk.sample_rate,
                channels: chunk.channels,
                samples: chunk.pcm,
            };
            if pcm_tx.send(pcm).await.is_err() {
                break;
            }
        }
    });
    let caps = tokio::task::spawn_blocking(etherquic_media::caps::host_caps)
        .await
        .map_err(|error| error.to_string())?;
    let options = host::HostOptions {
        fps: proto::CAPTURE_FPS,
        caps,
        log,
    };
    let closed = conn.closed();
    let result = tokio::select! {
        result = host::run(streams, negotiated, video, pcm_rx, options, cursor) => result,
        _ = closed => Ok(()),
    };
    forward.abort();
    ack_task.abort();
    input_task.abort();
    result
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
    let send = conn.open_uni().await.map_err(|error| error.to_string())?;
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
    let mut recv = conn.accept_uni().await.map_err(|error| error.to_string())?;
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
