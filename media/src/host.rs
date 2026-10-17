//! Host side of a session: read the player's offer, pick codecs, encode the
//! newest picture whenever the stream can take one, and switch codecs when an
//! encoder fails or the player sends a new offer.
//!
//! Capture writes into a [`FrameSlot`], which keeps only the newest raw
//! frame. The encoder pulls from it at its own pace, and QUIC flow control
//! pushes back on the encoder, so a slow link drops raw frames before they are
//! encoded. That keeps inter-frame codecs (H.264, H.265, AV1) valid: every
//! coded frame that leaves the encoder reaches the player.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use quinn::{Connection, RecvStream, SendStream};
use tokio::sync::{mpsc, watch};

use crate::audio::AudioEncoder;
use crate::negotiate::{
    choose_audio, choose_video, Audio, HostCaps, Offer, StreamConfig, Video, AUDIO2_MAGIC,
    OFFER_MAGIC, RECORD_CURSOR, RECORD_DATA, VIDEO2_MAGIC,
};
use crate::video::{EncodeSetup, VideoEncoder};
use crate::{wire, yuv, Frame, Log, Pcm};

pub const LEGACY_VIDEO_MAGIC: &[u8; 4] = b"EVID";
pub const LEGACY_AUDIO_MAGIC: &[u8; 4] = b"EAUD";

#[derive(Default)]
struct SlotState {
    frame: Option<Arc<Frame>>,
    seq: u64,
    closed: bool,
}

/// The newest captured frame. Older ones are replaced, never queued.
#[derive(Clone, Default)]
pub struct FrameSlot {
    inner: Arc<(Mutex<SlotState>, Condvar)>,
}

pub enum Wait {
    Frame(u64, Arc<Frame>),
    Timeout,
    Closed,
}

impl FrameSlot {
    pub fn new() -> FrameSlot {
        FrameSlot::default()
    }

    pub fn put(&self, frame: Frame) {
        let (lock, cond) = &*self.inner;
        if let Ok(mut state) = lock.lock() {
            state.frame = Some(Arc::new(frame));
            state.seq += 1;
            cond.notify_all();
        }
    }

    pub fn close(&self) {
        let (lock, cond) = &*self.inner;
        if let Ok(mut state) = lock.lock() {
            state.closed = true;
            cond.notify_all();
        }
    }

    pub fn is_closed(&self) -> bool {
        self.inner.0.lock().map(|s| s.closed).unwrap_or(true)
    }

    /// The newest frame after `seen`, waiting up to `timeout`.
    pub fn wait(&self, seen: u64, timeout: Duration) -> Wait {
        let (lock, cond) = &*self.inner;
        let Ok(mut state) = lock.lock() else {
            return Wait::Closed;
        };
        let deadline = Instant::now() + timeout;
        loop {
            if state.closed {
                return Wait::Closed;
            }
            if state.seq > seen {
                if let Some(frame) = &state.frame {
                    return Wait::Frame(state.seq, frame.clone());
                }
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Wait::Timeout;
            }
            match cond.wait_timeout(state, left) {
                Ok((next, _)) => state = next,
                Err(_) => return Wait::Closed,
            }
        }
    }
}

/// How this session carries media.
pub enum Negotiated {
    /// The peer did not agree on `etherquic/2`, or sent no offer: MJPEG and PCM.
    Legacy,
    V2 {
        offer: Offer,
        offers: RecvStream,
    },
}

impl Negotiated {
    pub fn is_v2(&self) -> bool {
        matches!(self, Negotiated::V2 { .. })
    }
}

/// With `etherquic/2` the player's first stream is its offer. Anything else
/// (or nothing within five seconds) is treated as a player that cannot
/// negotiate. A non-offer stream is handed back so the caller can still use it.
pub async fn accept_offer(
    conn: &Connection,
) -> Result<(Negotiated, Option<(RecvStream, [u8; 4])>), String> {
    if !etherquic::negotiates(conn) {
        return Ok((Negotiated::Legacy, None));
    }
    let accepted = tokio::time::timeout(Duration::from_secs(5), conn.accept_uni()).await;
    let mut recv = match accepted {
        Ok(Ok(recv)) => recv,
        Ok(Err(error)) => return Err(error.to_string()),
        Err(_) => return Ok((Negotiated::Legacy, None)),
    };
    let Some(magic) = wire::read_magic(&mut recv).await? else {
        return Ok((Negotiated::Legacy, None));
    };
    if &magic != OFFER_MAGIC {
        return Ok((Negotiated::Legacy, Some((recv, magic))));
    }
    let record = wire::read_record(&mut recv, OFFER_MAGIC)
        .await?
        .ok_or("offer stream ended")?;
    let offer = Offer::decode(&record)?;
    Ok((
        Negotiated::V2 {
            offer,
            offers: recv,
        },
        None,
    ))
}

/// The two host-to-player media streams.
pub struct Streams {
    video: SendStream,
    audio: SendStream,
    v2: bool,
}

pub async fn open_streams(conn: &Connection, v2: bool) -> Result<Streams, String> {
    let (vm, am) = if v2 {
        (VIDEO2_MAGIC, AUDIO2_MAGIC)
    } else {
        (LEGACY_VIDEO_MAGIC, LEGACY_AUDIO_MAGIC)
    };
    let mut video = conn.open_uni().await.map_err(|e| e.to_string())?;
    video
        .set_priority(etherquic::PRIORITY_VIDEO)
        .map_err(|e| e.to_string())?;
    wire::write_magic(&mut video, vm).await?;
    let mut audio = conn.open_uni().await.map_err(|e| e.to_string())?;
    audio
        .set_priority(etherquic::PRIORITY_AUDIO)
        .map_err(|e| e.to_string())?;
    wire::write_magic(&mut audio, am).await?;
    Ok(Streams { video, audio, v2 })
}

/// Host settings for one session.
#[derive(Clone)]
pub struct HostOptions {
    /// Capture rate, also the encoder's nominal rate.
    pub fps: u32,
    pub caps: HostCaps,
    pub log: Log,
}

enum VideoOut {
    Config(Vec<u8>),
    Data(Vec<u8>),
}

/// Send pictures and audio until capture stops (the slot closes and the
/// audio sender drops) or the player goes away.
pub async fn run(
    streams: Streams,
    negotiated: Negotiated,
    frames: FrameSlot,
    audio: mpsc::Receiver<Pcm>,
    options: HostOptions,
    mut cursor: watch::Receiver<Option<Vec<u8>>>,
) -> Result<(), String> {
    let Streams {
        video: mut video_send,
        audio: audio_send,
        v2,
    } = streams;
    let (offer_tx, offer_rx) = watch::channel::<Option<Offer>>(None);
    let mut offer_task = None;
    if let Negotiated::V2 { offer, mut offers } = negotiated {
        let _ = offer_tx.send(Some(offer));
        let log = options.log.clone();
        offer_task = Some(tokio::spawn(async move {
            loop {
                match wire::read_record(&mut offers, OFFER_MAGIC).await {
                    Ok(Some(record)) => match Offer::decode(&record) {
                        Ok(next) => {
                            log(&format!(
                                "player sent a new offer: video {:?} failed {:?} look {}",
                                tokens(&next.video),
                                tokens(&next.failed),
                                next.look.token()
                            ));
                            let _ = offer_tx.send(Some(next));
                        }
                        Err(error) => log(&format!("bad offer: {error}")),
                    },
                    Ok(None) | Err(_) => break,
                }
            }
        }));
    }

    let (out_tx, mut out_rx) = mpsc::channel::<VideoOut>(4);
    let sent = Arc::new(AtomicU64::new(0));
    let pump_frames = frames;
    let pump_options = options.clone();
    let pump_offer = offer_rx.clone();
    let pump_sent = sent.clone();
    let pump = std::thread::Builder::new()
        .name("etherquic-video-encode".into())
        .spawn(move || video_pump(pump_frames, pump_offer, pump_options, out_tx, pump_sent))
        .map_err(|e| e.to_string())?;

    let video_task = tokio::spawn(async move {
        let mut cursor_open = true;
        loop {
            tokio::select! {
                biased;
                // An encoded picture goes out before a cursor sample. The watch
                // keeps only the latest cursor, so the pointer still follows
                // this frame instead of sitting in front of it.
                out = out_rx.recv() => {
                    let Some(out) = out else { break };
                    write_out(&mut video_send, v2, out).await?;
                    if cursor_open {
                        cursor_open = write_pending_cursor(&mut cursor, &mut video_send, v2).await?;
                    }
                }
                changed = cursor.changed(), if cursor_open => {
                    match changed {
                        Ok(()) => {
                            cursor_open = send_cursor(&mut cursor, &mut video_send, v2).await?;
                        }
                        Err(_) => cursor_open = false,
                    }
                }
            }
        }
        finish(&mut video_send).await
    });

    let audio_task = tokio::spawn(audio_loop(audio_send, v2, audio, offer_rx, options.clone()));

    let video_result = video_task.await.map_err(|e| e.to_string()).and_then(|r| r);
    let audio_result = audio_task.await.map_err(|e| e.to_string()).and_then(|r| r);
    let _ = tokio::task::spawn_blocking(move || pump.join()).await;
    if let Some(task) = offer_task {
        task.abort();
    }
    video_result.and(audio_result)
}

async fn write_out(send: &mut SendStream, v2: bool, out: VideoOut) -> Result<(), String> {
    match out {
        VideoOut::Config(bytes) if v2 => wire::write_record(send, VIDEO2_MAGIC, &bytes).await,
        VideoOut::Config(_) => Ok(()),
        VideoOut::Data(bytes) if v2 => {
            wire::write_typed(send, VIDEO2_MAGIC, RECORD_DATA, &bytes).await
        }
        VideoOut::Data(bytes) => wire::write_record(send, LEGACY_VIDEO_MAGIC, &bytes).await,
    }
}

/// `false` when the cursor watch has closed.
async fn write_pending_cursor(
    cursor: &mut watch::Receiver<Option<Vec<u8>>>,
    send: &mut SendStream,
    v2: bool,
) -> Result<bool, String> {
    match cursor.has_changed() {
        Ok(false) => Ok(true),
        Err(_) => Ok(false),
        Ok(true) => send_cursor(cursor, send, v2).await,
    }
}

async fn send_cursor(
    cursor: &mut watch::Receiver<Option<Vec<u8>>>,
    send: &mut SendStream,
    v2: bool,
) -> Result<bool, String> {
    let bytes = cursor.borrow_and_update().clone();
    if v2 {
        if let Some(bytes) = bytes {
            wire::write_typed(send, VIDEO2_MAGIC, RECORD_CURSOR, &bytes).await?;
        }
    }
    Ok(true)
}

async fn finish(send: &mut SendStream) -> Result<(), String> {
    send.finish().map_err(|e| e.to_string())?;
    let _ = tokio::time::timeout(Duration::from_secs(2), send.stopped()).await;
    Ok(())
}

fn tokens(rows: &[Video]) -> Vec<&'static str> {
    rows.iter().map(|row| row.token()).collect()
}

struct Active {
    encoder: VideoEncoder,
    video: Video,
    size: (u32, u32),
}

fn video_pump(
    frames: FrameSlot,
    mut offer_rx: watch::Receiver<Option<Offer>>,
    options: HostOptions,
    out: mpsc::Sender<VideoOut>,
    sent: Arc<AtomicU64>,
) {
    let log = options.log.clone();
    let mut offer = offer_rx.borrow_and_update().clone();
    let v2 = offer.is_some();
    let mut excluded: Vec<Video> = Vec::new();
    let mut active: Option<Active> = None;
    let mut reason = String::new();
    let mut first_config = true;
    let mut seen = 0u64;
    let mut frames_in = 0u64;
    let mut tick = Instant::now();
    let mut tick_bytes = 0u64;
    loop {
        if offer_rx.has_changed().unwrap_or(false) {
            let next = offer_rx.borrow_and_update().clone();
            let keep_encoder = match (&offer, &next) {
                (Some(current), Some(incoming)) => current.same_codecs(incoming),
                (None, None) => true,
                _ => false,
            };
            offer = next;
            if !keep_encoder {
                active = None;
                reason = "the player sent a new offer".into();
            }
        }
        let frame = match frames.wait(seen, Duration::from_millis(250)) {
            Wait::Frame(seq, frame) => {
                seen = seq;
                frame
            }
            Wait::Timeout if out.is_closed() => return,
            Wait::Timeout => continue,
            Wait::Closed => break,
        };
        let (max_w, max_h) = offer
            .as_ref()
            .map(|o| (o.max.width, o.max.height))
            .unwrap_or((0, 0));
        let mut frame = yuv::fit(&frame, max_w, max_h);
        let look = offer.as_ref().map(|item| item.look).unwrap_or_default();
        etherquic::look::apply(&mut frame.rgb, frame.width, frame.height, look);
        if active
            .as_ref()
            .map(|a| a.size != (frame.width, frame.height))
            .unwrap_or(true)
        {
            // The old encoder (and an ffmpeg reader thread) ends before the
            // next config record, so no stale bytes follow it.
            drop(active.take());
            loop {
                let (video, encoder_name) = match &offer {
                    None => (Video::Mjpeg, "jpeg".to_string()),
                    Some(offer) => {
                        let pick = choose_video(
                            offer,
                            &options.caps,
                            &excluded,
                            Some((frame.width, frame.height)),
                        );
                        (pick.row, pick.encoder)
                    }
                };
                let kbps = offer.as_ref().map(|o| o.bitrate_kbps).unwrap_or(0);
                let setup = EncodeSetup {
                    video,
                    encoder: encoder_name.clone(),
                    width: frame.width,
                    height: frame.height,
                    fps: options.fps,
                    kbps,
                };
                let sink_out = out.clone();
                let sink_sent = sent.clone();
                let sink: crate::Sink<Vec<u8>> = Arc::new(move |bytes: Vec<u8>| {
                    sink_sent.fetch_add(bytes.len() as u64, Ordering::Relaxed);
                    sink_out.blocking_send(VideoOut::Data(bytes)).is_ok()
                });
                match VideoEncoder::open(&setup, sink) {
                    Ok((encoder, kbps)) => {
                        let mut config = StreamConfig::video(
                            video,
                            &encoder_name,
                            frame.width,
                            frame.height,
                            options.fps,
                        );
                        config.bitrate_kbps = kbps;
                        config.reason = std::mem::take(&mut reason);
                        if first_config {
                            config.host = options.caps.clone();
                            first_config = false;
                        }
                        log(&format!(
                            "video {} ({}) {}x{} {} fps{}{}",
                            video.name(),
                            encoder_name,
                            frame.width,
                            frame.height,
                            options.fps,
                            if kbps > 0 {
                                format!(" {kbps} kbit/s")
                            } else {
                                String::new()
                            },
                            if config.reason.is_empty() {
                                String::new()
                            } else {
                                format!(", because {}", config.reason)
                            },
                        ));
                        if v2
                            && out
                                .blocking_send(VideoOut::Config(config.encode()))
                                .is_err()
                        {
                            return;
                        }
                        active = Some(Active {
                            encoder,
                            video,
                            size: (frame.width, frame.height),
                        });
                        break;
                    }
                    Err(error) => {
                        log(&format!(
                            "{} encoder failed to start: {error}",
                            video.name()
                        ));
                        if video == Video::Mjpeg {
                            log("MJPEG cannot start; the picture stops.");
                            return;
                        }
                        excluded.push(video);
                        reason = format!("{} encoder failed: {error}", video.token());
                    }
                }
            }
        }
        let Some(current) = active.as_mut() else {
            continue;
        };
        match current.encoder.push(&frame) {
            Ok(true) => frames_in += 1,
            Ok(false) => return,
            Err(error) => {
                log(&format!(
                    "{} encoder failed mid-session: {error}",
                    current.video.name()
                ));
                if current.video == Video::Mjpeg {
                    continue;
                }
                excluded.push(current.video);
                reason = format!("{} encoder failed: {error}", current.video.token());
                active = None;
            }
        }
        if tick.elapsed() >= Duration::from_secs(5) {
            let total = sent.load(Ordering::Relaxed);
            let secs = tick.elapsed().as_secs_f64();
            if let Some(current) = &active {
                log(&format!(
                    "sending {} {}x{} {:.1} fps {:.2} Mbit/s",
                    current.video.token(),
                    current.size.0,
                    current.size.1,
                    frames_in as f64 / secs,
                    (total - tick_bytes) as f64 * 8.0 / secs / 1e6
                ));
            }
            tick = Instant::now();
            tick_bytes = total;
            frames_in = 0;
        }
    }
}

async fn audio_loop(
    mut send: SendStream,
    v2: bool,
    mut audio: mpsc::Receiver<Pcm>,
    mut offer_rx: watch::Receiver<Option<Offer>>,
    options: HostOptions,
) -> Result<(), String> {
    let log = options.log.clone();
    let mut offer = offer_rx.borrow_and_update().clone();
    let mut excluded: Vec<Audio> = Vec::new();
    let mut encoder: Option<(AudioEncoder, Audio)> = None;
    while let Some(pcm) = audio.recv().await {
        if !v2 {
            wire::write_record(&mut send, LEGACY_AUDIO_MAGIC, &pcm.encode()).await?;
            continue;
        }
        if offer_rx.has_changed().unwrap_or(false) {
            offer = offer_rx.borrow_and_update().clone();
            encoder = None;
        }
        if encoder
            .as_ref()
            .map(|(enc, _)| enc.changed(&pcm))
            .unwrap_or(true)
        {
            encoder = None;
            let mut reason = String::new();
            while encoder.is_none() {
                let pick = match &offer {
                    Some(offer) => choose_audio(offer, &options.caps, &excluded),
                    None => break,
                };
                match AudioEncoder::open(pick.row, pcm.sample_rate, pcm.channels) {
                    Ok((enc, rate, channels)) => {
                        let mut config =
                            StreamConfig::audio(pick.row, &pick.encoder, rate, channels);
                        config.reason = std::mem::take(&mut reason);
                        if pick.row == Audio::Opus {
                            config.bitrate_kbps = (crate::opus::DEFAULT_BITRATE / 1000) as u32;
                        }
                        log(&format!(
                            "audio {} {} Hz {} ch (capture {} Hz {} ch)",
                            pick.row.name(),
                            rate,
                            channels,
                            pcm.sample_rate,
                            pcm.channels
                        ));
                        wire::write_record(&mut send, AUDIO2_MAGIC, &config.encode()).await?;
                        encoder = Some((enc, pick.row));
                    }
                    Err(error) => {
                        log(&format!(
                            "{} audio encoder failed: {error}",
                            pick.row.name()
                        ));
                        if pick.row == Audio::Pcm {
                            return Err(error);
                        }
                        excluded.push(pick.row);
                        reason = format!("{} encoder failed: {error}", pick.row.token());
                    }
                }
            }
        }
        let Some((enc, row)) = encoder.as_mut() else {
            continue;
        };
        match enc.push(&pcm) {
            Ok(packets) => {
                for packet in packets {
                    wire::write_typed(&mut send, AUDIO2_MAGIC, RECORD_DATA, &packet).await?;
                }
            }
            Err(error) => {
                log(&format!(
                    "{} audio encoder failed mid-session: {error}",
                    row.name()
                ));
                excluded.push(*row);
                encoder = None;
            }
        }
    }
    finish(&mut send).await
}
