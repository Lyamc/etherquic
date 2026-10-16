//! Host and player roles on one Quinn connection.

use etherquic::{PRIORITY_INPUT, PRIORITY_TRACE};
use etherquic_input::{decode, encode, Event};
use quinn::{Connection, RecvStream, SendStream};
use tokio::sync::mpsc;

use crate::ack::{InputAck, ACK_MAGIC};
use crate::pipes::Pipes;
use crate::pipes::{
    read_chat, read_clipboard, read_files, write_chat, write_clipboard, write_files, CHAT_MAGIC,
    CLIP_MAGIC, FILE_MAGIC, NOTE_PRIORITY,
};
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

/// Media for a host session: the newest captured frame and audio chunks.
pub struct HostMedia {
    pub frames: etherquic_media::host::FrameSlot,
    pub audio: mpsc::Receiver<AudioChunk>,
    pub fps: u32,
    pub log: etherquic_media::Log,
    /// Latest host cursor record. `None` until the host has sampled one.
    pub cursor: tokio::sync::watch::Receiver<Option<Vec<u8>>>,
}

/// Send desktop and audio. Read player input until the player closes it.
///
/// A player that agreed on `etherquic/2` sends an offer first, and gets the
/// codecs both ends support; any other player gets MJPEG and PCM on the
/// original streams. This returns after media ends and the input stream has
/// ended.
pub async fn serve(
    conn: Connection,
    media: HostMedia,
    input: mpsc::Sender<Vec<Event>>,
    acks: Option<mpsc::Receiver<InputAck>>,
) -> Result<(), Error> {
    serve_session(conn, media, input, acks, Pipes::idle()).await
}

/// [`serve`] plus file, clipboard, and chat pipes.
///
/// The extra streams open only after both ends agreed on `etherquic/2`.
pub async fn serve_session(
    conn: Connection,
    media: HostMedia,
    input: mpsc::Sender<Vec<Event>>,
    acks: Option<mpsc::Receiver<InputAck>>,
    pipes: Pipes,
) -> Result<(), Error> {
    use etherquic_media::host;
    let wire_err = |error: String| Error::Wire(error);
    let (negotiated, stray) = host::accept_offer(&conn).await.map_err(wire_err)?;
    if let Some((_, magic)) = &stray {
        return Err(Error::Wire(format!(
            "expected an offer, got {}",
            magic_name(magic)
        )));
    }
    // Open picture and audio before reading input. A confirmation opened
    // earlier could be accepted in place of those two streams.
    let v2 = negotiated.is_v2();
    let streams = host::open_streams(&conn, v2).await.map_err(wire_err)?;
    if v2 {
        spawn_note_writers(&conn, pipes.files_out, pipes.clipboard_out, pipes.chat_out).await?;
    }
    let input_conn = conn.clone();
    let files_in = pipes.files_in;
    let clipboard_in = pipes.clipboard_in;
    let chat_in = pipes.chat_in;
    let roster = pipes.roster;
    let input_task = tokio::spawn(async move {
        read_from_player(
            input_conn,
            v2,
            input,
            files_in,
            clipboard_in,
            chat_in,
            roster,
        )
        .await
    });
    let ack_task = acks.map(|acks| {
        let ack_conn = conn.clone();
        tokio::spawn(async move { write_acks(ack_conn, acks).await })
    });
    let (pcm_tx, pcm_rx) = mpsc::channel(8);
    let mut audio = media.audio;
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
    let options = host::HostOptions {
        fps: media.fps.max(1),
        caps: etherquic_media::caps::host_caps(),
        log: media.log,
    };
    let result = host::run(
        streams,
        negotiated,
        media.frames,
        pcm_rx,
        options,
        media.cursor,
    )
    .await;
    forward.abort();
    if let Some(task) = ack_task {
        task.abort();
    }
    result.map_err(wire_err)?;
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

/// What the player asks for, and where session events go.
pub struct PlayerMedia {
    /// Sent when the host agreed on `etherquic/2`.
    pub offer: etherquic_media::negotiate::Offer,
    pub frames: etherquic_media::Sink<etherquic_media::Frame>,
    pub events: mpsc::UnboundedSender<etherquic_media::player::PlayerEvent>,
    /// Latest picture look. A change is written again on the offer stream.
    pub look: tokio::sync::watch::Receiver<etherquic::look::Look>,
}

async fn resend_look(
    offer: &mut etherquic_media::negotiate::Offer,
    look: etherquic::look::Look,
    offer_send: &mut Option<SendStream>,
) {
    if offer.look == look {
        return;
    }
    offer.look = look;
    let Some(send) = offer_send.as_mut() else {
        return;
    };
    if let Err(error) = etherquic_media::player::resend_offer(send, offer).await {
        eprintln!("etherquic-play: look: {error}");
    }
}

/// Receive desktop and audio. Forward player events until `input` closes
/// or both media streams end.
///
/// The two host streams are read at the same time. Reading one to the end
/// before accepting the other fills the unread stream and stalls the host.
pub async fn play(
    conn: Connection,
    media: PlayerMedia,
    audio: mpsc::Sender<AudioChunk>,
    input: mpsc::Receiver<Vec<Event>>,
    acks: Option<mpsc::Sender<InputAck>>,
) -> Result<(), Error> {
    play_session(conn, media, audio, input, acks, Pipes::idle()).await
}

/// [`play`] plus file, clipboard, and chat pipes.
pub async fn play_session(
    conn: Connection,
    media: PlayerMedia,
    audio: mpsc::Sender<AudioChunk>,
    input: mpsc::Receiver<Vec<Event>>,
    acks: Option<mpsc::Sender<InputAck>>,
    pipes: Pipes,
) -> Result<(), Error> {
    use etherquic_media::negotiate::{AUDIO2_MAGIC, VIDEO2_MAGIC};
    use etherquic_media::player::{self, PlayerEvent};
    let PlayerMedia {
        mut offer,
        frames,
        events,
        look,
    } = media;
    let negotiates = etherquic::negotiates(&conn);
    let mut offer_send = if negotiates {
        Some(
            player::send_offer(&conn, &offer)
                .await
                .map_err(Error::Wire)?,
        )
    } else {
        None
    };
    let (inner_tx, mut inner_rx) = mpsc::unbounded_channel::<PlayerEvent>();
    let (done_tx, mut done_rx) = mpsc::channel(2);
    let mut got_video = false;
    let mut got_audio = false;
    // File, clipboard, and chat are optional host streams. A desktop host
    // (the Windows service included) sends only picture and audio. Waiting
    // for the other three leaves this task blocked in accept_uni, so the
    // EINP stream never opens and the remote keyboard and mouse stay dead
    // while the picture is already on screen.
    let mut got_file = false;
    let mut got_clip = false;
    let mut got_chat = false;
    let mut early_ack = None;
    let (pcm_tx, mut pcm_rx) = mpsc::channel::<etherquic_media::Pcm>(8);
    tokio::spawn(async move {
        while let Some(pcm) = pcm_rx.recv().await {
            let chunk = AudioChunk {
                sample_rate: pcm.sample_rate,
                channels: pcm.channels,
                pcm: pcm.samples,
            };
            if audio.send(chunk).await.is_err() {
                break;
            }
        }
    });
    while !got_video || !got_audio {
        let mut recv = conn
            .accept_uni()
            .await
            .map_err(|error| Error::Wire(error.to_string()))?;
        let magic = wire::read_magic(&mut recv).await?;
        let done = done_tx.clone();
        let is_video = &magic == VIDEO_MAGIC || &magic == VIDEO2_MAGIC;
        let is_audio = &magic == AUDIO_MAGIC || &magic == AUDIO2_MAGIC;
        if is_video && !got_video {
            got_video = true;
            let v2 = &magic == VIDEO2_MAGIC;
            let frames = frames.clone();
            let events = inner_tx.clone();
            tokio::spawn(async move {
                let result = player::read_video(recv, v2, frames, events)
                    .await
                    .map_err(Error::Wire);
                let _ = done.send(result).await;
            });
        } else if is_audio && !got_audio {
            got_audio = true;
            let v2 = &magic == AUDIO2_MAGIC;
            let pcm = pcm_tx.clone();
            let events = inner_tx.clone();
            tokio::spawn(async move {
                let result = player::read_audio(recv, v2, pcm, events)
                    .await
                    .map_err(Error::Wire);
                let _ = done.send(result).await;
            });
        } else if &magic == ACK_MAGIC && early_ack.is_none() {
            early_ack = Some(recv);
        } else if negotiates && &magic == FILE_MAGIC && !got_file {
            got_file = true;
            let files_in = pipes.files_in.clone();
            tokio::spawn(async move {
                if let Err(error) = read_files(recv, files_in).await {
                    eprintln!("etherquic-play: file: {error}");
                }
            });
        } else if negotiates && &magic == CLIP_MAGIC && !got_clip {
            got_clip = true;
            let clipboard_in = pipes.clipboard_in.clone();
            tokio::spawn(async move {
                if let Err(error) = read_clipboard(recv, clipboard_in).await {
                    eprintln!("etherquic-play: clipboard: {error}");
                }
            });
        } else if negotiates && &magic == CHAT_MAGIC && !got_chat {
            got_chat = true;
            let chat_in = pipes.chat_in.clone();
            let roster = pipes.roster.clone();
            tokio::spawn(async move {
                if let Err(error) = read_chat(recv, roster, chat_in).await {
                    eprintln!("etherquic-play: chat: {error}");
                }
            });
        } else {
            return Err(Error::Wire(format!(
                "unknown stream {}",
                magic_name(&magic)
            )));
        }
    }
    drop(pcm_tx);
    drop(inner_tx);
    // A decoder that breaks sends a new offer without that codec. The host
    // answers with a config record for the next codec on the same stream.
    // A picture-look change uses that same stream and leaves the encoder up.
    tokio::spawn(async move {
        let mut look_rx = look;
        let seen = *look_rx.borrow_and_update();
        resend_look(&mut offer, seen, &mut offer_send).await;
        let mut watch_look = true;
        loop {
            let event = if watch_look {
                tokio::select! {
                    event = inner_rx.recv() => event,
                    changed = look_rx.changed() => {
                        if changed.is_err() {
                            watch_look = false;
                        } else {
                            let next = *look_rx.borrow_and_update();
                            resend_look(&mut offer, next, &mut offer_send).await;
                        }
                        continue;
                    }
                }
            } else {
                inner_rx.recv().await
            };
            let Some(event) = event else {
                break;
            };
            if let PlayerEvent::VideoFailed { video, reason } = &event {
                eprintln!("etherquic-play: {} decoder failed: {reason}", video.name());
                if !offer.failed.contains(video) {
                    offer.failed.push(*video);
                }
                if let Some(send) = offer_send.as_mut() {
                    if let Err(error) = player::resend_offer(send, &offer).await {
                        eprintln!("etherquic-play: renegotiate: {error}");
                    }
                }
            }
            if let PlayerEvent::AudioFailed { audio, reason } = &event {
                eprintln!(
                    "etherquic-play: {} audio decoder failed: {reason}",
                    audio.name()
                );
                offer.audio.retain(|row| row != audio);
                if let Some(send) = offer_send.as_mut() {
                    let _ = player::resend_offer(send, &offer).await;
                }
            }
            if events.send(event).is_err() && offer_send.is_none() {
                break;
            }
        }
    });
    // Side streams and input confirmations can arrive after the picture.
    // Reading them here, not in the loop above, is what lets EINP open when
    // the host has no file, clipboard, or chat stream.
    {
        let more = conn.clone();
        let files_in = pipes.files_in.clone();
        let clipboard_in = pipes.clipboard_in.clone();
        let chat_in = pipes.chat_in.clone();
        let roster = pipes.roster.clone();
        tokio::spawn(async move {
            if let Err(error) = accept_late(
                more,
                negotiates,
                got_file,
                got_clip,
                got_chat,
                early_ack,
                acks,
                files_in,
                clipboard_in,
                chat_in,
                roster,
            )
            .await
            {
                eprintln!("etherquic-play: side stream: {error}");
            }
        });
    }
    // Input opens after the picture streams so the host can send those first.
    // Notes follow input, so an older host that accepts one stream still sees EINP.
    let mut input_send = open_uni(&conn, PRIORITY_INPUT, INPUT_MAGIC).await?;
    if negotiates {
        spawn_note_writers(&conn, pipes.files_out, pipes.clipboard_out, pipes.chat_out).await?;
    }
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

/// File, clipboard, chat, and input confirmations that were not on the
/// first two host streams. Returns when every stream this session still
/// expects has been accepted, or the connection closes.
async fn accept_late(
    conn: Connection,
    negotiates: bool,
    have_file: bool,
    have_clip: bool,
    have_chat: bool,
    mut ack_recv: Option<RecvStream>,
    acks: Option<mpsc::Sender<InputAck>>,
    files_in: mpsc::Sender<crate::transfer::NamedFile>,
    clipboard_in: mpsc::Sender<String>,
    chat_in: mpsc::Sender<crate::chat::Routed>,
    roster: Vec<crate::chat::Participant>,
) -> Result<(), Error> {
    let mut need_file = negotiates && !have_file;
    let mut need_clip = negotiates && !have_clip;
    let mut need_chat = negotiates && !have_chat;
    let mut acks = acks;
    while need_file || need_clip || need_chat || (acks.is_some() && ack_recv.is_none()) {
        let mut recv = match conn.accept_uni().await {
            Ok(recv) => recv,
            // The desktop host often never opens these streams. The
            // connection ending is that case, not a failed session.
            Err(_) => break,
        };
        let magic = match wire::read_magic(&mut recv).await {
            Ok(magic) => magic,
            Err(_) => break,
        };
        if &magic == FILE_MAGIC && need_file {
            need_file = false;
            let files_in = files_in.clone();
            tokio::spawn(async move {
                if let Err(error) = read_files(recv, files_in).await {
                    eprintln!("etherquic-play: file: {error}");
                }
            });
        } else if &magic == CLIP_MAGIC && need_clip {
            need_clip = false;
            let clipboard_in = clipboard_in.clone();
            tokio::spawn(async move {
                if let Err(error) = read_clipboard(recv, clipboard_in).await {
                    eprintln!("etherquic-play: clipboard: {error}");
                }
            });
        } else if &magic == CHAT_MAGIC && need_chat {
            need_chat = false;
            let chat_in = chat_in.clone();
            let roster = roster.clone();
            tokio::spawn(async move {
                if let Err(error) = read_chat(recv, roster, chat_in).await {
                    eprintln!("etherquic-play: chat: {error}");
                }
            });
        } else if &magic == ACK_MAGIC && ack_recv.is_none() {
            ack_recv = Some(recv);
        } else {
            eprintln!("etherquic-play: ignored stream {}", magic_name(&magic));
        }
    }
    if let (Some(acks), Some(recv)) = (acks.take(), ack_recv) {
        read_ack_records(recv, acks).await?;
    }
    Ok(())
}

async fn read_ack_records(
    mut recv: RecvStream,
    acks: mpsc::Sender<InputAck>,
) -> Result<(), Error> {
    while let Some(payload) = wire::read_record(&mut recv, ACK_MAGIC).await? {
        let ack = InputAck::decode(&payload)?;
        if acks.send(ack).await.is_err() {
            break;
        }
    }
    Ok(())
}

async fn spawn_note_writers(
    conn: &Connection,
    files_out: mpsc::Receiver<crate::transfer::NamedFile>,
    clipboard_out: mpsc::Receiver<String>,
    chat_out: mpsc::Receiver<crate::chat::ChatMessage>,
) -> Result<(), Error> {
    let mut file_send = open_uni(conn, etherquic::PRIORITY_VIDEO, FILE_MAGIC).await?;
    tokio::spawn(async move {
        if let Err(error) = write_files(&mut file_send, files_out).await {
            eprintln!("etherquic-play: file send: {error}");
        }
    });
    let mut clip_send = open_uni(conn, NOTE_PRIORITY, CLIP_MAGIC).await?;
    tokio::spawn(async move {
        if let Err(error) = write_clipboard(&mut clip_send, clipboard_out).await {
            eprintln!("etherquic-play: clipboard send: {error}");
        }
    });
    let mut chat_send = open_uni(conn, NOTE_PRIORITY, CHAT_MAGIC).await?;
    tokio::spawn(async move {
        if let Err(error) = write_chat(&mut chat_send, chat_out).await {
            eprintln!("etherquic-play: chat send: {error}");
        }
    });
    Ok(())
}

async fn read_from_player(
    conn: Connection,
    v2: bool,
    input: mpsc::Sender<Vec<Event>>,
    files_in: mpsc::Sender<crate::transfer::NamedFile>,
    clipboard_in: mpsc::Sender<String>,
    chat_in: mpsc::Sender<crate::chat::Routed>,
    roster: Vec<crate::chat::Participant>,
) -> Result<(), Error> {
    if !v2 {
        return read_input(conn, input).await;
    }
    let mut input_task = None;
    let mut file_task = None;
    let mut clip_task = None;
    let mut chat_task = None;
    while input_task.is_none() || file_task.is_none() || clip_task.is_none() || chat_task.is_none()
    {
        let mut recv = conn
            .accept_uni()
            .await
            .map_err(|error| Error::Wire(error.to_string()))?;
        let magic = wire::read_magic(&mut recv).await?;
        if &magic == INPUT_MAGIC && input_task.is_none() {
            let input = input.clone();
            input_task = Some(tokio::spawn(
                async move { read_input_stream(recv, input).await },
            ));
        } else if &magic == FILE_MAGIC && file_task.is_none() {
            let files_in = files_in.clone();
            file_task = Some(tokio::spawn(
                async move { read_files(recv, files_in).await },
            ));
        } else if &magic == CLIP_MAGIC && clip_task.is_none() {
            let clipboard_in = clipboard_in.clone();
            clip_task = Some(tokio::spawn(async move {
                read_clipboard(recv, clipboard_in).await
            }));
        } else if &magic == CHAT_MAGIC && chat_task.is_none() {
            let chat_in = chat_in.clone();
            let roster = roster.clone();
            chat_task = Some(tokio::spawn(async move {
                read_chat(recv, roster, chat_in).await
            }));
        } else {
            return Err(Error::Wire(format!(
                "unexpected stream {}",
                magic_name(&magic)
            )));
        }
    }
    for task in [input_task, file_task, clip_task, chat_task]
        .into_iter()
        .flatten()
    {
        match task.await {
            Ok(result) => result?,
            Err(error) => return Err(Error::Wire(error.to_string())),
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
    read_input_stream(recv, input).await
}

async fn read_input_stream(
    mut recv: RecvStream,
    input: mpsc::Sender<Vec<Event>>,
) -> Result<(), Error> {
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
