//! Host and player roles on one Quinn connection.

use etherquic::{PRIORITY_AUDIO, PRIORITY_INPUT, PRIORITY_TRACE, PRIORITY_VIDEO};
use etherquic_input::{decode, encode, Event};
use quinn::{Connection, RecvStream, SendStream};
use tokio::sync::mpsc;

use crate::ack::{InputAck, ACK_MAGIC};
use crate::wire::{self, AUDIO_MAGIC, INPUT_MAGIC, VIDEO_MAGIC};
use crate::Error;

/// Interleaved little-endian PCM.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AudioChunk {
    pub sample_rate: u32,
    pub channels: u16,
    pub pcm: Vec<i16>,
}

impl AudioChunk {
    pub fn encode(&self) -> Result<Vec<u8>, Error> {
        if self.channels == 0 {
            return Err(Error::Wire("audio has no channels".into()));
        }
        if self.pcm.len() % self.channels as usize != 0 {
            return Err(Error::Wire(
                "audio length is not a whole number of frames".into(),
            ));
        }
        let mut out = Vec::with_capacity(6 + self.pcm.len() * 2);
        out.extend_from_slice(&self.sample_rate.to_le_bytes());
        out.extend_from_slice(&self.channels.to_le_bytes());
        for sample in &self.pcm {
            out.extend_from_slice(&sample.to_le_bytes());
        }
        Ok(out)
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, Error> {
        if bytes.len() < 6 {
            return Err(Error::Wire("audio record is short".into()));
        }
        let sample_rate = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
        let channels = u16::from_le_bytes([bytes[4], bytes[5]]);
        if channels == 0 {
            return Err(Error::Wire("audio has no channels".into()));
        }
        let pcm_bytes = &bytes[6..];
        if pcm_bytes.len() % 2 != 0 {
            return Err(Error::Wire("audio pcm is not i16".into()));
        }
        let pcm: Vec<i16> = pcm_bytes
            .chunks_exact(2)
            .map(|pair| i16::from_le_bytes([pair[0], pair[1]]))
            .collect();
        if pcm.len() % channels as usize != 0 {
            return Err(Error::Wire(
                "audio length is not a whole number of frames".into(),
            ));
        }
        Ok(Self {
            sample_rate,
            channels,
            pcm,
        })
    }
}

/// Mix to mono, resample, then duplicate into `out_channels`.
pub fn resample(chunk: &AudioChunk, out_channels: u16, out_rate: u32) -> Result<Vec<i16>, Error> {
    if chunk.channels == 0 || out_channels == 0 || chunk.sample_rate == 0 || out_rate == 0 {
        return Err(Error::Wire("audio rate or channel count is zero".into()));
    }
    let in_channels = chunk.channels as usize;
    if chunk.pcm.is_empty() {
        return Ok(Vec::new());
    }
    let frames = chunk.pcm.len() / in_channels;
    let mut mono = Vec::with_capacity(frames);
    for frame in chunk.pcm.chunks_exact(in_channels) {
        let sum: i32 = frame.iter().map(|sample| i32::from(*sample)).sum();
        mono.push((sum / in_channels as i32) as i16);
    }
    let out_frames =
        ((u64::from(out_rate) * mono.len() as u64) / u64::from(chunk.sample_rate)).max(1) as usize;
    let mut stretched = Vec::with_capacity(out_frames);
    if mono.len() == 1 || out_frames == 1 {
        stretched.push(mono[0]);
    } else {
        for i in 0..out_frames {
            let pos = (i as u64 * (mono.len() as u64 - 1)) / (out_frames as u64 - 1).max(1);
            let pos = (pos as usize).min(mono.len() - 1);
            stretched.push(mono[pos]);
        }
    }
    let mut out = Vec::with_capacity(stretched.len() * out_channels as usize);
    for sample in stretched {
        for _ in 0..out_channels {
            out.push(sample);
        }
    }
    Ok(out)
}

/// Send desktop and audio. Read player input until the player closes it.
///
/// `video` payloads are JPEG bytes. Closing either sender finishes that
/// stream. This returns after both streams are finished and the input
/// stream has ended.
pub async fn serve(
    conn: Connection,
    mut video: mpsc::Receiver<Vec<u8>>,
    mut audio: mpsc::Receiver<AudioChunk>,
    input: mpsc::Sender<Vec<Event>>,
    acks: Option<mpsc::Receiver<InputAck>>,
) -> Result<(), Error> {
    // Open picture and audio before reading input. A confirmation opened
    // earlier could be accepted in place of those two streams.
    let mut video_send = open_uni(&conn, PRIORITY_VIDEO, VIDEO_MAGIC).await?;
    let mut audio_send = open_uni(&conn, PRIORITY_AUDIO, AUDIO_MAGIC).await?;
    let input_conn = conn.clone();
    let input_task = tokio::spawn(async move { read_input(input_conn, input).await });
    let ack_task = acks.map(|acks| {
        let ack_conn = conn.clone();
        tokio::spawn(async move { write_acks(ack_conn, acks).await })
    });
    let mut video_open = true;
    let mut audio_open = true;
    while video_open || audio_open {
        tokio::select! {
            biased;
            msg = video.recv(), if video_open => {
                match msg {
                    Some(jpeg) => wire::write_record(&mut video_send, VIDEO_MAGIC, &jpeg).await?,
                    None => {
                        finish(&mut video_send).await?;
                        video_open = false;
                    }
                }
            }
            msg = audio.recv(), if audio_open => {
                match msg {
                    Some(chunk) => {
                        let payload = chunk.encode()?;
                        wire::write_record(&mut audio_send, AUDIO_MAGIC, &payload).await?;
                    }
                    None => {
                        finish(&mut audio_send).await?;
                        audio_open = false;
                    }
                }
            }
        }
    }
    if let Some(task) = ack_task {
        task.abort();
    }
    match input_task.await {
        Ok(result) => result,
        Err(error) => Err(Error::Wire(error.to_string())),
    }
}

/// Write confirmations as they arrive. This is its own task so a picture
/// frame cannot sit in front of the timing report.
async fn write_acks(conn: Connection, mut acks: mpsc::Receiver<InputAck>) -> Result<(), Error> {
    let mut send = None;
    while let Some(ack) = acks.recv().await {
        if send.is_none() {
            send = Some(open_uni(&conn, PRIORITY_TRACE, ACK_MAGIC).await?);
        }
        if let Some(stream) = send.as_mut() {
            wire::write_record(stream, ACK_MAGIC, &ack.encode()).await?;
        }
    }
    if let Some(stream) = send.as_mut() {
        finish(stream).await?;
    }
    Ok(())
}

/// Receive desktop and audio. Forward player events until `input` closes
/// or both media streams end.
///
/// The two host streams are read at the same time. Reading one to the end
/// before accepting the other fills the unread stream and stalls the host.
pub async fn play(
    conn: Connection,
    video: mpsc::Sender<Vec<u8>>,
    audio: mpsc::Sender<AudioChunk>,
    input: mpsc::Receiver<Vec<Event>>,
    acks: Option<mpsc::Sender<InputAck>>,
) -> Result<(), Error> {
    let (done_tx, mut done_rx) = mpsc::channel(2);
    let mut got_video = false;
    let mut got_audio = false;
    let mut early_ack = None;
    while !got_video || !got_audio {
        let mut recv = conn
            .accept_uni()
            .await
            .map_err(|error| Error::Wire(error.to_string()))?;
        let magic = wire::read_magic(&mut recv).await?;
        let done = done_tx.clone();
        if &magic == VIDEO_MAGIC && !got_video {
            got_video = true;
            let video = video.clone();
            tokio::spawn(async move {
                let result = read_video(recv, video).await;
                let _ = done.send(result).await;
            });
        } else if &magic == AUDIO_MAGIC && !got_audio {
            got_audio = true;
            let audio = audio.clone();
            tokio::spawn(async move {
                let result = read_audio(recv, audio).await;
                let _ = done.send(result).await;
            });
        } else if &magic == ACK_MAGIC && early_ack.is_none() {
            early_ack = Some(recv);
        } else {
            return Err(Error::Wire(format!(
                "unknown stream {}",
                magic_name(&magic)
            )));
        }
    }
    if let Some(acks) = acks {
        let ack_conn = conn.clone();
        tokio::spawn(async move {
            if let Err(error) = read_acks(ack_conn, early_ack, acks).await {
                eprintln!("etherquic-play: input trace: {error}");
            }
        });
    }
    // Input opens after the picture streams so the host can send those first.
    let mut input_send = open_uni(&conn, PRIORITY_INPUT, INPUT_MAGIC).await?;
    let (stop_tx, mut stop_rx) = mpsc::channel::<()>(1);
    let writer =
        tokio::spawn(async move { write_input(&mut input_send, input, &mut stop_rx).await });
    drop(done_tx);
    for _ in 0..2 {
        done_rx.recv().await.ok_or(Error::Closed)??;
    }
    drop(stop_tx);
    match writer.await {
        Ok(result) => result,
        Err(error) => Err(Error::Wire(error.to_string())),
    }
}

async fn open_uni(conn: &Connection, priority: i32, magic: &[u8; 4]) -> Result<SendStream, Error> {
    let send = conn
        .open_uni()
        .await
        .map_err(|error| Error::Wire(error.to_string()))?;
    send.set_priority(priority)
        .map_err(|error| Error::Wire(error.to_string()))?;
    let mut send = send;
    wire::write_magic(&mut send, magic).await?;
    Ok(send)
}

async fn finish(send: &mut SendStream) -> Result<(), Error> {
    send.finish()
        .map_err(|error| Error::Wire(error.to_string()))?;
    // Wait until the peer has the bytes. Dropping the connection before that
    // turns a clean end of stream into "connection lost" on the reader.
    send.stopped()
        .await
        .map_err(|error| Error::Wire(error.to_string()))?;
    Ok(())
}

async fn read_acks(
    conn: Connection,
    early: Option<RecvStream>,
    acks: mpsc::Sender<InputAck>,
) -> Result<(), Error> {
    let mut recv = if let Some(recv) = early {
        recv
    } else {
        loop {
            let mut incoming = conn
                .accept_uni()
                .await
                .map_err(|error| Error::Wire(error.to_string()))?;
            let magic = wire::read_magic(&mut incoming).await?;
            if &magic == ACK_MAGIC {
                break incoming;
            }
        }
    };
    while let Some(payload) = wire::read_record(&mut recv, ACK_MAGIC).await? {
        let ack = InputAck::decode(&payload)?;
        if acks.send(ack).await.is_err() {
            break;
        }
    }
    Ok(())
}

async fn read_input(conn: Connection, input: mpsc::Sender<Vec<Event>>) -> Result<(), Error> {
    let mut recv = conn
        .accept_uni()
        .await
        .map_err(|error| Error::Wire(error.to_string()))?;
    let magic = wire::read_magic(&mut recv).await?;
    if &magic != INPUT_MAGIC {
        return Err(Error::Wire(format!(
            "expected input stream, got {}",
            magic_name(&magic)
        )));
    }
    while let Some(payload) = wire::read_record(&mut recv, INPUT_MAGIC).await? {
        let events = decode(&payload).map_err(|error| Error::Input(error.to_string()))?;
        if input.send(events).await.is_err() {
            break;
        }
    }
    Ok(())
}

async fn write_input(
    send: &mut SendStream,
    mut input: mpsc::Receiver<Vec<Event>>,
    stop: &mut mpsc::Receiver<()>,
) -> Result<(), Error> {
    loop {
        tokio::select! {
            biased;
            _ = stop.recv() => break,
            msg = input.recv() => {
                match msg {
                    Some(events) if !events.is_empty() => {
                        let payload = encode(&events).map_err(|error| Error::Input(error.to_string()))?;
                        wire::write_record(send, INPUT_MAGIC, &payload).await?;
                    }
                    Some(_) => {}
                    None => break,
                }
            }
        }
    }
    finish(send).await
}

async fn read_video(mut recv: RecvStream, video: mpsc::Sender<Vec<u8>>) -> Result<(), Error> {
    while let Some(jpeg) = wire::read_record(&mut recv, VIDEO_MAGIC).await? {
        if video.send(jpeg).await.is_err() {
            break;
        }
    }
    Ok(())
}

async fn read_audio(mut recv: RecvStream, audio: mpsc::Sender<AudioChunk>) -> Result<(), Error> {
    while let Some(payload) = wire::read_record(&mut recv, AUDIO_MAGIC).await? {
        let chunk = AudioChunk::decode(&payload)?;
        if audio.send(chunk).await.is_err() {
            break;
        }
    }
    Ok(())
}

fn magic_name(magic: &[u8; 4]) -> String {
    String::from_utf8_lossy(magic).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn audio_chunk_round_trips() {
        let chunk = AudioChunk {
            sample_rate: 48_000,
            channels: 2,
            pcm: vec![1, -1, 2, -2],
        };
        let back = AudioChunk::decode(&chunk.encode().unwrap()).unwrap();
        assert_eq!(back, chunk);
    }

    #[test]
    fn resample_duplicates_a_mono_sample_into_stereo() {
        let chunk = AudioChunk {
            sample_rate: 8_000,
            channels: 1,
            pcm: vec![1000],
        };
        let out = resample(&chunk, 2, 8_000).unwrap();
        assert_eq!(out, vec![1000, 1000]);
    }
}
