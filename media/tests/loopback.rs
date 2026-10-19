//! A host and a player on one machine over real QUIC.

use std::net::{Ipv4Addr, SocketAddr, UdpSocket};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use etherquic_media::host::{self, FrameSlot, HostOptions};
use etherquic_media::negotiate::{
    Audio, HostCaps, Limit, Offer, Video, VideoCodec, AUDIO2_MAGIC, VIDEO2_MAGIC,
};
use etherquic_media::player::{self, PlayerEvent};
use etherquic_media::{wire, Frame, Pcm};

fn certs() -> (PathBuf, PathBuf) {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../certs");
    (root.join("cert.pem"), root.join("key.pem"))
}

fn free_port() -> u16 {
    loop {
        let probe = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).unwrap();
        let port = probe.local_addr().unwrap().port();
        if port < u16::MAX - etherquic::UDP_PORT_TRIES {
            return port;
        }
    }
}

fn frame(i: u32) -> Frame {
    let (w, h) = (320u32, 180u32);
    let mut rgb = vec![0u8; (w * h * 3) as usize];
    for (p, px) in rgb.chunks_exact_mut(3).enumerate() {
        px[0] = (p as u32 + i * 3) as u8;
        px[1] = (p as u32 / 320) as u8;
        px[2] = i as u8;
    }
    Frame {
        width: w,
        height: h,
        rgb,
    }
}

struct Outcome {
    negotiates: bool,
    first_video: Option<Video>,
    videos: Vec<Video>,
    audio: Option<Audio>,
    frames: u64,
    pcm_frames: u64,
}

/// Run a host with `caps` and a player that offers `offer` (or only the old
/// ALPN when `legacy`). With `fail_first` the player reports the first codec
/// as undecodable, which must move the host to the next row.
async fn session(caps: HostCaps, offer: Offer, legacy: bool, fail_first: bool) -> Outcome {
    etherquic::install_pure_crypto();
    let (cert, key) = certs();
    let listeners = etherquic::udp_listeners(free_port(), &cert, &key);
    let server = listeners.v4.clone().expect("listener");
    let port = listeners.port;

    let host_task = tokio::spawn(async move {
        let conn = server.accept().await.unwrap().await.unwrap();
        let (negotiated, _) = host::accept_offer(&conn).await.unwrap();
        let streams = host::open_streams(&conn, negotiated.is_v2()).await.unwrap();
        let slot = FrameSlot::new();
        let feeder_slot = slot.clone();
        let feeder = std::thread::spawn(move || {
            for i in 0..60 {
                feeder_slot.put(frame(i));
                std::thread::sleep(Duration::from_millis(33));
            }
            feeder_slot.close();
        });
        let (pcm_tx, pcm_rx) = tokio::sync::mpsc::channel(8);
        tokio::spawn(async move {
            for _ in 0..50 {
                let pcm = Pcm {
                    sample_rate: 48_000,
                    channels: 2,
                    samples: vec![0i16; 960 * 2],
                };
                if pcm_tx.send(pcm).await.is_err() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        });
        let options = HostOptions {
            fps: etherquic::negotiate::FpsWish::Fixed(30),
            display_hz: None,
            caps,
            log: Arc::new(|line: &str| eprintln!("host: {line}")),
        };
        let (_cursor_tx, cursor_rx) = tokio::sync::watch::channel(None);
        let result = host::run(streams, negotiated, slot, pcm_rx, options, cursor_rx).await;
        let _ = feeder.join();
        result
    });

    let endpoint = etherquic::udp_client_endpoint_on("127.0.0.1:0".parse().unwrap()).unwrap();
    let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    let conn = if legacy {
        let mut tls = etherquic::tls::load_client_config(true).unwrap();
        tls.alpn_protocols = vec![etherquic::ALPN_ETHERQUIC.to_vec()];
        let quic = quinn::crypto::rustls::QuicClientConfig::try_from(tls).unwrap();
        endpoint
            .connect_with(quinn::ClientConfig::new(Arc::new(quic)), addr, "localhost")
            .unwrap()
            .await
            .unwrap()
    } else {
        endpoint.connect(addr, "localhost").unwrap().await.unwrap()
    };
    let negotiates = etherquic::negotiates(&conn);
    let mut offer_send = if negotiates {
        Some(player::send_offer(&conn, &offer).await.unwrap())
    } else {
        None
    };
    let frames = Arc::new(AtomicU64::new(0));
    let f = frames.clone();
    let sink: etherquic_media::Sink<Frame> = Arc::new(move |_| {
        f.fetch_add(1, Ordering::Relaxed);
        true
    });
    let (events_tx, mut events_rx) = tokio::sync::mpsc::unbounded_channel();
    let (pcm_tx, mut pcm_rx) = tokio::sync::mpsc::channel::<Pcm>(64);
    let pcm_frames = Arc::new(AtomicU64::new(0));
    let pf = pcm_frames.clone();
    tokio::spawn(async move {
        while let Some(pcm) = pcm_rx.recv().await {
            pf.fetch_add(
                (pcm.samples.len() / pcm.channels as usize) as u64,
                Ordering::Relaxed,
            );
        }
    });
    let mut readers = Vec::new();
    for _ in 0..2 {
        let mut recv = conn.accept_uni().await.unwrap();
        let magic = wire::read_magic(&mut recv).await.unwrap().unwrap();
        if &magic == b"EVID" || &magic == VIDEO2_MAGIC {
            let v2 = &magic == VIDEO2_MAGIC;
            assert_eq!(v2, negotiates);
            readers.push(tokio::spawn(player::read_video(
                recv,
                v2,
                sink.clone(),
                events_tx.clone(),
            )));
        } else {
            let v2 = &magic == AUDIO2_MAGIC;
            assert_eq!(v2, negotiates);
            readers.push(tokio::spawn(player::read_audio(
                recv,
                v2,
                pcm_tx.clone(),
                events_tx.clone(),
            )));
        }
    }
    drop(events_tx);
    drop(pcm_tx);
    let mut out = Outcome {
        negotiates,
        first_video: None,
        videos: Vec::new(),
        audio: None,
        frames: 0,
        pcm_frames: 0,
    };
    let mut offer = offer;
    while let Ok(Some(event)) =
        tokio::time::timeout(Duration::from_secs(10), events_rx.recv()).await
    {
        match event {
            PlayerEvent::Video(config) => {
                let video = config.video.unwrap();
                if out.first_video.is_none() {
                    out.first_video = Some(video);
                    if fail_first {
                        offer.failed.push(video);
                        player::resend_offer(offer_send.as_mut().unwrap(), &offer)
                            .await
                            .unwrap();
                    }
                }
                out.videos.push(video);
            }
            PlayerEvent::Audio(config) => out.audio = config.audio,
            _ => {}
        }
    }
    for reader in readers {
        let _ = reader.await;
    }
    let _ = host_task.await;
    out.frames = frames.load(Ordering::Relaxed);
    out.pcm_frames = pcm_frames.load(Ordering::Relaxed);
    if out.first_video.is_none() && !negotiates {
        out.first_video = Some(Video::Mjpeg);
    }
    conn.close(0u32.into(), b"done");
    out
}

fn mjpeg_only_offer(video: &[Video]) -> Offer {
    etherquic_media::negotiate::build_offer(&etherquic_media::negotiate::OfferInput {
        video_order: video,
        audio_order: &[Audio::Pcm],
        decodes: &[VideoCodec::Mjpeg],
        ..Default::default()
    })
}

#[tokio::test(flavor = "multi_thread")]
async fn negotiated_mjpeg_and_pcm_flow() {
    let out = session(
        HostCaps::baseline(),
        mjpeg_only_offer(&Video::ALL),
        false,
        false,
    )
    .await;
    assert!(out.negotiates);
    assert_eq!(out.first_video, Some(Video::Mjpeg));
    assert_eq!(out.audio, Some(Audio::Pcm));
    assert!(out.frames > 20, "frames {}", out.frames);
    assert!(out.pcm_frames > 10_000, "pcm {}", out.pcm_frames);
}

#[tokio::test(flavor = "multi_thread")]
async fn old_player_gets_legacy_streams() {
    let out = session(
        etherquic_media::caps::host_caps(),
        mjpeg_only_offer(&Video::ALL),
        true,
        false,
    )
    .await;
    assert!(!out.negotiates);
    assert!(out.frames > 20, "frames {}", out.frames);
    assert!(out.pcm_frames > 10_000, "pcm {}", out.pcm_frames);
}

/// Uses whatever this computer really has: the best common codec first, then
/// a decoder failure report moves the host down the list.
#[tokio::test(flavor = "multi_thread")]
async fn real_codecs_and_renegotiation() {
    let probe = etherquic_media::caps::probe();
    eprintln!("{}", etherquic_media::caps::summary(probe));
    let offer = player::offer_for(
        &Video::ALL,
        &Audio::ALL,
        None,
        None,
        Limit::NONE,
        0,
        0,
        false,
        etherquic_media::negotiate::YuvFormat::YUV444P8,
        etherquic_media::negotiate::FpsWish::Auto,
    );
    let expect =
        etherquic_media::negotiate::choose_video(&offer, &probe.caps, &[], Some((320, 180))).row;
    let out = session(probe.caps.clone(), offer.clone(), false, true).await;
    assert_eq!(out.first_video, Some(expect));
    let want_audio = etherquic_media::negotiate::choose_audio(&offer, &probe.caps, &[]).row;
    assert_eq!(out.audio, Some(want_audio));
    if expect != Video::Mjpeg {
        assert!(out.videos.len() >= 2, "no renegotiation: {:?}", out.videos);
        assert_ne!(out.videos[1], expect);
    }
    assert!(out.frames > 15, "frames {}", out.frames);
    eprintln!(
        "videos {:?} audio {:?} frames {} pcm {}",
        out.videos, out.audio, out.frames, out.pcm_frames
    );
}
