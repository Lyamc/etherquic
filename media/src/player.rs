//! Player side of a session: send the offer, decode whatever the host
//! configures, and ask for another codec when a decoder breaks.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use quinn::{Connection, RecvStream, SendStream};
use tokio::sync::mpsc;

use crate::audio::AudioDecoder;
use crate::caps;
use crate::dirty;
use crate::negotiate::{
    build_offer, decode_clock, Audio, Limit, Offer, OfferInput, StreamConfig, Video, VideoCodec,
    CLOCK_AUDIO, CLOCK_VIDEO, OFFER_MAGIC, RECORD_CLOCK, RECORD_CONFIG, RECORD_CURSOR, RECORD_DATA,
    RECORD_PATCH,
};
use crate::video::VideoDecoder;
use crate::{jpeg, jpeg2000, wire, Frame, Pcm, Sink};

/// What the player decodes, from [`caps::probe`].
pub fn decodes() -> (Vec<VideoCodec>, Vec<Audio>) {
    let probe = caps::probe();
    (probe.decodes.clone(), probe.audio_decodes.clone())
}

/// The offer for this player from its Settings order and any pin.
pub fn offer_for(
    video_order: &[Video],
    audio_order: &[Audio],
    video_pin: Option<Video>,
    audio_pin: Option<Audio>,
    max: Limit,
    bitrate_kbps: u32,
    quality: u8,
    want_dirty_rects: bool,
    yuv: crate::negotiate::YuvFormat,
    fps: crate::negotiate::FpsWish,
    audio_bitrate_kbps: u32,
    audio_channels: u16,
    view_only: bool,
    exclusive: bool,
    carrier: Option<u8>,
) -> Offer {
    let (video, audio) = decodes();
    build_offer(&OfferInput {
        video_order,
        audio_order,
        video_pin,
        audio_pin,
        decodes: &video,
        audio_decodes: &audio,
        max,
        bitrate_kbps,
        quality,
        want_dirty_rects,
        yuv,
        fps,
        audio_bitrate_kbps,
        audio_channels,
        view_only,
        exclusive,
        carrier,
    })
}

/// Open the offer stream and send the first offer. Keep the stream: a later
/// offer on it renegotiates.
pub async fn send_offer(conn: &Connection, offer: &Offer) -> Result<SendStream, String> {
    let mut send = conn.open_uni().await.map_err(|e| e.to_string())?;
    send.set_priority(etherquic::PRIORITY_INPUT)
        .map_err(|e| e.to_string())?;
    wire::write_magic(&mut send, OFFER_MAGIC).await?;
    wire::write_record(&mut send, OFFER_MAGIC, &offer.encode()).await?;
    Ok(send)
}

pub async fn resend_offer(send: &mut SendStream, offer: &Offer) -> Result<(), String> {
    wire::write_record(send, OFFER_MAGIC, &offer.encode()).await
}

/// Things the session status shows.
#[derive(Clone, Debug)]
pub enum PlayerEvent {
    Video(StreamConfig),
    Audio(StreamConfig),
    VideoFailed {
        video: Video,
        reason: String,
    },
    AudioFailed {
        audio: Audio,
        reason: String,
    },
    Stats(Stats),
    /// Host cursor. Older sessions never send one.
    Cursor(etherquic::cursor::CursorUpdate),
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Stats {
    pub video: Option<Video>,
    pub width: u32,
    pub height: u32,
    pub fps: f64,
    pub video_mbps: f64,
}

enum DecodeIn {
    Config(StreamConfig),
    Data(Vec<u8>),
    Patch(Vec<u8>),
}

/// Read pictures. `v2` is an `EVI2` stream; otherwise it is the legacy `EVID`
/// JPEG stream. Decoded frames go to `frames`.
/// `Err` when an `etherquic/2` config echoes a different carrier than the offer.
/// The caller stops before the first picture.
pub fn carrier_echo_ok(v2: bool, expected: Option<u8>, echoed: Option<u8>) -> Result<(), String> {
    if !v2 {
        return Ok(());
    }
    etherquic::carrier::config_matches(true, expected, echoed).map(|_| ())
}

/// One task owns `read_datagram` and splits picture (lane 1) from audio (lane 2).
/// Assembled bytes are the same record body a stream reader would see.
pub fn spawn_lane_demux(
    conn: Connection,
    picture: mpsc::Sender<Vec<u8>>,
    audio: mpsc::Sender<Vec<u8>>,
) -> etherquic::carrier::AssemblerReset {
    let reset = etherquic::carrier::AssemblerReset::new();
    let flag = reset.clone();
    tokio::spawn(async move {
        let mut pictures = etherquic::carrier::Assembler::new();
        let mut audios = etherquic::carrier::Assembler::new();
        let mut seen = flag.get();
        loop {
            seen = etherquic::carrier::apply_reset(&mut pictures, flag.get(), seen);
            seen = etherquic::carrier::apply_reset(&mut audios, flag.get(), seen);
            let packet = match conn.read_datagram().await {
                Ok(packet) => packet,
                Err(_) => break,
            };
            seen = etherquic::carrier::apply_reset(&mut pictures, flag.get(), seen);
            seen = etherquic::carrier::apply_reset(&mut audios, flag.get(), seen);
            let Some((&lane, rest)) = packet.split_first() else {
                continue;
            };
            let Some(slice) = etherquic::carrier::Slice::decode(rest) else {
                continue;
            };
            let now = std::time::Instant::now();
            let deadline = now + Duration::from_secs(2);
            let (asm, tx) = if lane == etherquic::carrier::Lane::Picture as u8 {
                (&mut pictures, &picture)
            } else if lane == etherquic::carrier::Lane::Audio as u8 {
                (&mut audios, &audio)
            } else {
                continue;
            };
            if let Some(record) = asm.push(slice, now, deadline) {
                if tx.send(record).await.is_err() {
                    break;
                }
            }
        }
    });
    reset
}

pub async fn read_video(
    mut recv: RecvStream,
    v2: bool,
    frames: Sink<Frame>,
    events: mpsc::UnboundedSender<PlayerEvent>,
    expected_carrier: Option<u8>,
    mut pictures: Option<mpsc::Receiver<Vec<u8>>>,
    reset: Option<etherquic::carrier::AssemblerReset>,
) -> Result<(), String> {
    let magic: &[u8; 4] = if v2 {
        crate::negotiate::VIDEO2_MAGIC
    } else {
        crate::host::LEGACY_VIDEO_MAGIC
    };
    let decoded = Arc::new(AtomicU64::new(0));
    let mut configs = 0u32;
    let (tx, mut rx) = mpsc::channel::<DecodeIn>(8);
    let thread_events = events.clone();
    let thread_decoded = decoded.clone();
    let decoder_thread = std::thread::Builder::new()
        .name("etherquic-video-decode".into())
        .spawn(move || {
            let canvas: Arc<Mutex<Option<Frame>>> = Arc::new(Mutex::new(None));
            let counted: Sink<Frame> = {
                let frames = frames.clone();
                let decoded = thread_decoded.clone();
                let canvas = canvas.clone();
                Arc::new(move |frame: Frame| {
                    if let Ok(mut slot) = canvas.lock() {
                        *slot = Some(frame.clone());
                    }
                    decoded.fetch_add(1, Ordering::Relaxed);
                    frames(frame)
                })
            };
            let mut decoder: Option<(VideoDecoder, Video)> = if v2 {
                None
            } else {
                Some((
                    VideoDecoder::Mjpeg {
                        sink: counted.clone(),
                    },
                    Video::Mjpeg,
                ))
            };
            // A picture can win the race against the config record. Keep the
            // latest one and decode it once a decoder exists.
            let mut held: Option<Vec<u8>> = None;
            while let Some(input) = rx.blocking_recv() {
                match input {
                    DecodeIn::Config(config) => {
                        decoder = None;
                        if let Ok(mut slot) = canvas.lock() {
                            *slot = None;
                        }
                        let Some(video) = config.video else { continue };
                        match VideoDecoder::open(
                            video,
                            config.width,
                            config.height,
                            counted.clone(),
                            config.yuv,
                        ) {
                            Ok(dec) => {
                                decoder = Some((dec, video));
                                if let Some(bytes) = held.take() {
                                    let failed = decoder.as_mut().and_then(|(dec, video)| {
                                        dec.push(&bytes).err().map(|reason| (*video, reason))
                                    });
                                    if let Some((video, reason)) = failed {
                                        if video.skip_bad_frame() {
                                            eprintln!("etherquic-play: picture: {reason}");
                                            if video == Video::Mjpeg {
                                                decoder = Some((
                                                    VideoDecoder::Mjpeg {
                                                        sink: counted.clone(),
                                                    },
                                                    Video::Mjpeg,
                                                ));
                                            }
                                        } else {
                                            decoder = None;
                                            let _ = thread_events.send(PlayerEvent::VideoFailed {
                                                video,
                                                reason,
                                            });
                                        }
                                    }
                                }
                            }
                            Err(reason) => {
                                let _ =
                                    thread_events.send(PlayerEvent::VideoFailed { video, reason });
                            }
                        }
                    }
                    DecodeIn::Patch(bytes) => {
                        let Some((_, video)) = decoder.as_ref() else {
                            continue;
                        };
                        let mut slot = match canvas.lock() {
                            Ok(slot) => slot,
                            Err(_) => continue,
                        };
                        match apply_patch(&mut slot, *video, &bytes) {
                            Ok(Some(frame)) => {
                                drop(slot);
                                counted(frame);
                            }
                            Ok(None) => {}
                            Err(reason) => {
                                eprintln!("etherquic-play: picture patch: {reason}");
                            }
                        }
                    }
                    DecodeIn::Data(bytes) => {
                        let Some((dec, video)) = decoder.as_mut() else {
                            held = Some(bytes);
                            continue;
                        };
                        if let Err(reason) = dec.push(&bytes) {
                            let video = *video;
                            // MJPEG, MJ2K, and PyroWave are intra: one bad
                            // record is skipped. Falling back to MJPEG for a
                            // short PyroWave frame is what the AN515 sessions did.
                            if video.skip_bad_frame() {
                                eprintln!("etherquic-play: picture: {reason}");
                                if video == Video::Mjpeg {
                                    decoder = Some((
                                        VideoDecoder::Mjpeg {
                                            sink: counted.clone(),
                                        },
                                        Video::Mjpeg,
                                    ));
                                }
                            } else {
                                decoder = None;
                                let _ =
                                    thread_events.send(PlayerEvent::VideoFailed { video, reason });
                            }
                        }
                    }
                }
            }
        })
        .map_err(|e| e.to_string())?;

    let mut opened: Option<(Video, u32, u32)> = None;
    let mut stats = Stats::default();
    if !v2 {
        stats.video = Some(Video::Mjpeg);
    }
    let mut tick = Instant::now();
    let mut bytes = 0u64;
    let mut last_decoded = 0u64;
    let mut on_datagrams = pictures.is_some();
    let mut stream_open = true;
    let result = loop {
        enum Incoming {
            Stream(Vec<u8>),
            Datagram(Vec<u8>),
            Closed,
            Ended,
        }
        let incoming = if let Some(rx) = pictures.as_mut() {
            if stream_open {
                tokio::select! {
                    biased;
                    record = wire::read_record(&mut recv, magic) => {
                        match record {
                            Ok(Some(record)) => Incoming::Stream(record),
                            Ok(None) => Incoming::Ended,
                            Err(error) => break Err(error),
                        }
                    }
                    body = rx.recv() => match body {
                        Some(record) => Incoming::Datagram(record),
                        None => Incoming::Closed,
                    },
                }
            } else {
                // The reliable stream has finished. Carriers 3–5 still deliver
                // pictures on this channel. Leave once it goes quiet so the
                // task does not sit until the player thread's own timer.
                match tokio::time::timeout(Duration::from_millis(200), rx.recv()).await {
                    Ok(Some(record)) => Incoming::Datagram(record),
                    Ok(None) => Incoming::Closed,
                    Err(_) => break Ok(()),
                }
            }
        } else if stream_open {
            match wire::read_record(&mut recv, magic).await {
                Ok(Some(record)) => Incoming::Stream(record),
                Ok(None) => break Ok(()),
                Err(error) => break Err(error),
            }
        } else {
            break Ok(());
        };
        if matches!(incoming, Incoming::Ended) {
            if pictures.is_some() {
                stream_open = false;
                continue;
            }
            break Ok(());
        }
        if matches!(incoming, Incoming::Closed) {
            pictures = None;
            on_datagrams = false;
            if !stream_open {
                break Ok(());
            }
            continue;
        }
        let (record, from_datagram) = match incoming {
            Incoming::Stream(record) => (record, false),
            Incoming::Datagram(record) => (record, true),
            Incoming::Closed | Incoming::Ended => continue,
        };
        if on_datagrams && !from_datagram && matches!(record.first(), Some(&RECORD_DATA | &RECORD_PATCH))
        {
            continue;
        }
        bytes += record.len() as u64;
        let input = if !v2 {
            DecodeIn::Data(record)
        } else {
            match record.first() {
                Some(&RECORD_CONFIG) => match StreamConfig::decode(&record) {
                    Ok(config) => {
                        if let Err(error) =
                            carrier_echo_ok(v2, expected_carrier, config.carrier)
                        {
                            break Err(error);
                        }
                        // The config is on the reliable stream and copied onto
                        // RTP. Opening the decoder twice drops the first
                        // keyframe, and H.264 then stays blank until the next
                        // GOP. The second copy is the same picture.
                        if opened == Some((config.video.unwrap_or(Video::Mjpeg), config.width, config.height))
                            && config.video.is_some()
                        {
                            continue;
                        }
                        opened = config.video.map(|video| (video, config.width, config.height));
                        etherquic::carrier::pulse_on_later_config(&mut configs, reset.as_ref());
                        on_datagrams = config.carrier.is_some_and(|id| {
                            id == etherquic::carrier::QUIC_DATAGRAMS || id >= etherquic::carrier::UDP_RTP
                        });
                        stats.video = config.video;
                        stats.width = config.width;
                        stats.height = config.height;
                        let _ = events.send(PlayerEvent::Video(config.clone()));
                        DecodeIn::Config(config)
                    }
                    Err(error) => {
                        eprintln!("etherquic-play: video config: {error}");
                        continue;
                    }
                },
                Some(&RECORD_DATA) => DecodeIn::Data(record[1..].to_vec()),
                Some(&RECORD_PATCH) => DecodeIn::Patch(record[1..].to_vec()),
                Some(&RECORD_CURSOR) => {
                    if let Ok(update) = etherquic::cursor::CursorUpdate::decode(&record[1..]) {
                        let _ = events.send(PlayerEvent::Cursor(update));
                    }
                    continue;
                }
                Some(&RECORD_CLOCK) => {
                    if let Some((stream, delay)) = decode_clock(&record) {
                        if stream == CLOCK_VIDEO {
                            crate::timing::push_video(delay);
                        }
                    }
                    continue;
                }
                _ => continue,
            }
        };
        if tx.send(input).await.is_err() {
            break Ok(());
        }
        let elapsed = tick.elapsed();
        if elapsed >= Duration::from_secs(2) {
            let now = decoded.load(Ordering::Relaxed);
            stats.fps = (now - last_decoded) as f64 / elapsed.as_secs_f64();
            stats.video_mbps = bytes as f64 * 8.0 / elapsed.as_secs_f64() / 1e6;
            let _ = events.send(PlayerEvent::Stats(stats.clone()));
            last_decoded = now;
            bytes = 0;
            tick = Instant::now();
        }
    };
    drop(tx);
    let _ = tokio::task::spawn_blocking(move || decoder_thread.join()).await;
    result
}

fn apply_patch(
    canvas: &mut Option<Frame>,
    video: Video,
    bytes: &[u8],
) -> Result<Option<Frame>, String> {
    let (rect, coded) = dirty::decode_payload(bytes)?;
    let tile = match video {
        Video::Mjpeg => jpeg::decode(coded)?,
        Video::Mj2k => jpeg2000::decode(coded)?,
        _ => return Err("this codec does not take a tile".into()),
    };
    if tile.width != rect.w || tile.height != rect.h {
        return Err("patch size does not match its rectangle".into());
    }
    let Some(picture) = canvas.as_mut() else {
        return Ok(None);
    };
    dirty::blit(picture, &tile, rect.x, rect.y)?;
    Ok(Some(picture.clone()))
}

/// ST 2110-30 is L16. The negotiated row can still say Opus; decoding those
/// sample bytes as an Opus packet is the noise.
pub fn audio_decoder_row(audio: Audio, carrier: Option<u8>) -> Audio {
    if carrier == Some(etherquic::carrier::ST2110) {
        Audio::Pcm
    } else {
        audio
    }
}

/// Read audio from `EAU2` (or the legacy `EAUD` PCM stream).
pub async fn read_audio(
    mut recv: RecvStream,
    v2: bool,
    pcm: mpsc::Sender<Pcm>,
    events: mpsc::UnboundedSender<PlayerEvent>,
    mut packets: Option<mpsc::Receiver<Vec<u8>>>,
    reset: Option<etherquic::carrier::AssemblerReset>,
) -> Result<(), String> {
    let magic: &[u8; 4] = if v2 {
        crate::negotiate::AUDIO2_MAGIC
    } else {
        crate::host::LEGACY_AUDIO_MAGIC
    };
    let mut decoder: Option<(AudioDecoder, Audio)> = if v2 {
        None
    } else {
        Some((AudioDecoder::Pcm, Audio::Pcm))
    };
    let mut configs = 0u32;
    let mut on_datagrams = packets.is_some();
    let mut stream_open = true;
    let mut held: Option<Vec<u8>> = None;
    loop {
        enum Incoming {
            Stream(Vec<u8>),
            Datagram(Vec<u8>),
            Closed,
            Ended,
        }
        let incoming = if let Some(rx) = packets.as_mut() {
            if stream_open {
                tokio::select! {
                    biased;
                    record = wire::read_record(&mut recv, magic) => {
                        match record {
                            Ok(Some(record)) => Incoming::Stream(record),
                            Ok(None) => Incoming::Ended,
                            Err(error) => return Err(error),
                        }
                    }
                    body = rx.recv() => match body {
                        Some(record) => Incoming::Datagram(record),
                        None => Incoming::Closed,
                    },
                }
            } else {
                match tokio::time::timeout(Duration::from_millis(200), rx.recv()).await {
                    Ok(Some(record)) => Incoming::Datagram(record),
                    Ok(None) => Incoming::Closed,
                    Err(_) => break,
                }
            }
        } else if stream_open {
            match wire::read_record(&mut recv, magic).await? {
                Some(record) => Incoming::Stream(record),
                None => break,
            }
        } else {
            break;
        };
        if matches!(incoming, Incoming::Ended) {
            if packets.is_some() {
                stream_open = false;
                continue;
            }
            break;
        }
        if matches!(incoming, Incoming::Closed) {
            packets = None;
            on_datagrams = false;
            if !stream_open {
                break;
            }
            continue;
        }
        let (record, from_datagram) = match incoming {
            Incoming::Stream(record) => (record, false),
            Incoming::Datagram(record) => (record, true),
            Incoming::Closed | Incoming::Ended => continue,
        };
        if on_datagrams && !from_datagram && record.first() == Some(&RECORD_DATA) {
            continue;
        }
        let data = if !v2 {
            record
        } else {
            match record.first() {
                Some(&RECORD_CONFIG) => {
                    match StreamConfig::decode(&record) {
                        Ok(config) => {
                            etherquic::carrier::pulse_on_later_config(&mut configs, reset.as_ref());
                            on_datagrams = config.carrier.is_some_and(|id| {
                                id == etherquic::carrier::QUIC_DATAGRAMS
                                    || id >= etherquic::carrier::UDP_RTP
                            });
                            let _ = events.send(PlayerEvent::Audio(config.clone()));
                            decoder = None;
                            if let Some(audio) = config.audio {
                                let audio = audio_decoder_row(audio, config.carrier);
                                match AudioDecoder::open(audio, config.channels.max(1)) {
                                    Ok(dec) => {
                                        decoder = Some((dec, audio));
                                        if let Some(bytes) = held.take() {
                                            let decoded = decoder.as_mut().map(|(dec, audio)| {
                                                dec.decode(&bytes).map_err(|reason| (*audio, reason))
                                            });
                                            match decoded {
                                                Some(Ok(chunk)) => {
                                                    if pcm.send(chunk).await.is_err() {
                                                        break;
                                                    }
                                                }
                                                Some(Err((audio, reason))) => {
                                                    decoder = None;
                                                    let _ = events.send(PlayerEvent::AudioFailed {
                                                        audio,
                                                        reason,
                                                    });
                                                }
                                                None => {}
                                            }
                                        }
                                    }
                                    Err(reason) => {
                                        let _ =
                                            events.send(PlayerEvent::AudioFailed { audio, reason });
                                    }
                                }
                            }
                        }
                        Err(error) => eprintln!("etherquic-play: audio config: {error}"),
                    }
                    continue;
                }
                Some(&RECORD_DATA) => record[1..].to_vec(),
                Some(&RECORD_CLOCK) => {
                    if let Some((stream, delay)) = decode_clock(&record) {
                        if stream == CLOCK_AUDIO {
                            crate::timing::push_audio(delay);
                        }
                    }
                    continue;
                }
                _ => continue,
            }
        };
        let Some((dec, audio)) = decoder.as_mut() else {
            held = Some(data);
            continue;
        };
        match dec.decode(&data) {
            Ok(chunk) => {
                if pcm.send(chunk).await.is_err() {
                    break;
                }
            }
            Err(reason) => {
                let audio = *audio;
                decoder = None;
                let _ = events.send(PlayerEvent::AudioFailed { audio, reason });
            }
        }
    }
    Ok(())
}
