//! Player side of a session: send the offer, decode whatever the host
//! configures, and ask for another codec when a decoder breaks.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use quinn::{Connection, RecvStream, SendStream};
use tokio::sync::mpsc;

use crate::audio::AudioDecoder;
use crate::caps;
use crate::negotiate::{
    build_offer, Audio, Limit, Offer, OfferInput, StreamConfig, Video, VideoCodec, OFFER_MAGIC,
    RECORD_CONFIG, RECORD_CURSOR, RECORD_DATA,
};
use crate::video::VideoDecoder;
use crate::{wire, Frame, Pcm, Sink};

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
}

/// Read pictures. `v2` is an `EVI2` stream; otherwise it is the legacy `EVID`
/// JPEG stream. Decoded frames go to `frames`.
pub async fn read_video(
    mut recv: RecvStream,
    v2: bool,
    frames: Sink<Frame>,
    events: mpsc::UnboundedSender<PlayerEvent>,
) -> Result<(), String> {
    let magic: &[u8; 4] = if v2 {
        crate::negotiate::VIDEO2_MAGIC
    } else {
        crate::host::LEGACY_VIDEO_MAGIC
    };
    let decoded = Arc::new(AtomicU64::new(0));
    let (tx, mut rx) = mpsc::channel::<DecodeIn>(8);
    let thread_events = events.clone();
    let thread_decoded = decoded.clone();
    let decoder_thread = std::thread::Builder::new()
        .name("etherquic-video-decode".into())
        .spawn(move || {
            let counted: Sink<Frame> = {
                let frames = frames.clone();
                let decoded = thread_decoded.clone();
                Arc::new(move |frame: Frame| {
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
            while let Some(input) = rx.blocking_recv() {
                match input {
                    DecodeIn::Config(config) => {
                        decoder = None;
                        let Some(video) = config.video else { continue };
                        match VideoDecoder::open(
                            video,
                            config.width,
                            config.height,
                            counted.clone(),
                        ) {
                            Ok(dec) => decoder = Some((dec, video)),
                            Err(reason) => {
                                let _ =
                                    thread_events.send(PlayerEvent::VideoFailed { video, reason });
                            }
                        }
                    }
                    DecodeIn::Data(bytes) => {
                        let Some((dec, video)) = decoder.as_mut() else {
                            continue;
                        };
                        if let Err(reason) = dec.push(&bytes) {
                            let video = *video;
                            decoder = None;
                            // MJPEG is the floor: one bad JPEG is skipped, not renegotiated.
                            if video == Video::Mjpeg {
                                eprintln!("etherquic-play: picture: {reason}");
                                decoder = Some((
                                    VideoDecoder::Mjpeg {
                                        sink: counted.clone(),
                                    },
                                    Video::Mjpeg,
                                ));
                            } else {
                                let _ =
                                    thread_events.send(PlayerEvent::VideoFailed { video, reason });
                            }
                        }
                    }
                }
            }
        })
        .map_err(|e| e.to_string())?;

    let mut stats = Stats::default();
    if !v2 {
        stats.video = Some(Video::Mjpeg);
    }
    let mut tick = Instant::now();
    let mut bytes = 0u64;
    let mut last_decoded = 0u64;
    let result = loop {
        let record = match wire::read_record(&mut recv, magic).await {
            Ok(Some(record)) => record,
            Ok(None) => break Ok(()),
            Err(error) => break Err(error),
        };
        bytes += record.len() as u64;
        let input = if !v2 {
            DecodeIn::Data(record)
        } else {
            match record.first() {
                Some(&RECORD_CONFIG) => match StreamConfig::decode(&record) {
                    Ok(config) => {
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
                Some(&RECORD_CURSOR) => {
                    if let Ok(update) = etherquic::cursor::CursorUpdate::decode(&record[1..]) {
                        let _ = events.send(PlayerEvent::Cursor(update));
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

/// Read audio from `EAU2` (or the legacy `EAUD` PCM stream).
pub async fn read_audio(
    mut recv: RecvStream,
    v2: bool,
    pcm: mpsc::Sender<Pcm>,
    events: mpsc::UnboundedSender<PlayerEvent>,
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
    while let Some(record) = wire::read_record(&mut recv, magic).await? {
        let data: &[u8] = if !v2 {
            &record
        } else {
            match record.first() {
                Some(&RECORD_CONFIG) => {
                    match StreamConfig::decode(&record) {
                        Ok(config) => {
                            let _ = events.send(PlayerEvent::Audio(config.clone()));
                            decoder = None;
                            if let Some(audio) = config.audio {
                                match AudioDecoder::open(audio, config.channels.max(1)) {
                                    Ok(dec) => decoder = Some((dec, audio)),
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
                Some(&RECORD_DATA) => &record[1..],
                _ => continue,
            }
        };
        let Some((dec, audio)) = decoder.as_mut() else {
            continue;
        };
        match dec.decode(data) {
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
