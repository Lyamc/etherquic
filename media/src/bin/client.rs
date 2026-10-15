//! Headless player for measurements: dial a host over UDP, negotiate,
//! decode for N seconds, print what was used.
//!
//! `eqmedia-client IP:PORT SECONDS [--video a,b] [--audio a,b] [--legacy]`

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use etherquic_media::negotiate::{Audio, Limit, Video, AUDIO2_MAGIC, VIDEO2_MAGIC};
use etherquic_media::player::{self, PlayerEvent};
use etherquic_media::{wire, Frame};

#[tokio::main]
async fn main() -> Result<(), String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let addr: std::net::SocketAddr = args.first().ok_or("usage: eqmedia-client IP:PORT SECONDS")?.parse().map_err(|e| format!("{e}"))?;
    let secs: u64 = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(15);
    let mut video_order = Video::ALL.to_vec();
    let mut audio_order = Audio::ALL.to_vec();
    let mut legacy = false;
    let mut i = 2;
    while i < args.len() {
        match args[i].as_str() {
            "--video" => {
                video_order = args[i + 1].split(',').filter_map(Video::from_token).collect();
                i += 1;
            }
            "--audio" => {
                audio_order = args[i + 1].split(',').filter_map(Audio::from_token).collect();
                i += 1;
            }
            "--legacy" => legacy = true,
            other => return Err(format!("unknown arg {other}")),
        }
        i += 1;
    }
    etherquic::install_pure_crypto();
    let started = Instant::now();
    let probe = etherquic_media::caps::probe();
    eprintln!("probe {:?}: {}", started.elapsed(), etherquic_media::caps::summary(probe));
    let bind: std::net::SocketAddr = if addr.is_ipv4() { "0.0.0.0:0".parse().unwrap() } else { "[::]:0".parse().unwrap() };
    let endpoint = etherquic::udp_client_endpoint_on(bind).map_err(|e| e.to_string())?;
    let conn = if legacy {
        let mut tls = etherquic::tls::load_client_config(true).map_err(|e| e.to_string())?;
        tls.alpn_protocols = vec![etherquic::ALPN_ETHERQUIC.to_vec()];
        let quic = quinn::crypto::rustls::QuicClientConfig::try_from(tls).map_err(|e| e.to_string())?;
        endpoint.connect_with(quinn::ClientConfig::new(Arc::new(quic)), addr, "localhost").map_err(|e| e.to_string())?.await
    } else {
        endpoint.connect(addr, "localhost").map_err(|e| e.to_string())?.await
    }
    .map_err(|e| e.to_string())?;
    let negotiates = etherquic::negotiates(&conn);
    eprintln!("connected {addr}, alpn {:?}, negotiates {negotiates}", etherquic::agreed_alpn(&conn).map(|a| String::from_utf8_lossy(&a).into_owned()));
    let offer = player::offer_for(&video_order, &audio_order, None, None, Limit::NONE, 0);
    let mut offer_send = None;
    if negotiates {
        eprintln!("offer video {:?} audio {:?}", offer.video.iter().map(|v| v.token()).collect::<Vec<_>>(), offer.audio.iter().map(|a| a.token()).collect::<Vec<_>>());
        offer_send = Some(player::send_offer(&conn, &offer).await?);
    }
    let frames = Arc::new(AtomicU64::new(0));
    let f = frames.clone();
    let sink: etherquic_media::Sink<Frame> = Arc::new(move |_frame| {
        f.fetch_add(1, Ordering::Relaxed);
        true
    });
    let (events_tx, mut events_rx) = tokio::sync::mpsc::unbounded_channel();
    let (pcm_tx, mut pcm_rx) = tokio::sync::mpsc::channel(64);
    let audio_samples = Arc::new(AtomicU64::new(0));
    let a = audio_samples.clone();
    tokio::spawn(async move {
        while let Some(pcm) = pcm_rx.recv().await {
            let pcm: etherquic_media::Pcm = pcm;
            a.fetch_add((pcm.samples.len() / pcm.channels.max(1) as usize) as u64, Ordering::Relaxed);
        }
    });
    let mut got = 0;
    while got < 2 {
        let mut recv = conn.accept_uni().await.map_err(|e| e.to_string())?;
        let magic = wire::read_magic(&mut recv).await?.ok_or("stream ended")?;
        match &magic {
            b"EVID" | b"EVI2" => {
                let v2 = &magic == VIDEO2_MAGIC;
                let (sink, events) = (sink.clone(), events_tx.clone());
                tokio::spawn(async move { player::read_video(recv, v2, sink, events).await });
            }
            b"EAUD" | b"EAU2" => {
                let v2 = &magic == AUDIO2_MAGIC;
                let (pcm, events) = (pcm_tx.clone(), events_tx.clone());
                tokio::spawn(async move { player::read_audio(recv, v2, pcm, events).await });
            }
            other => return Err(format!("unexpected stream {}", String::from_utf8_lossy(other))),
        }
        got += 1;
    }
    // Hosts read input after opening media; an empty input stream keeps them happy.
    let mut input = conn.open_uni().await.map_err(|e| e.to_string())?;
    wire::write_magic(&mut input, b"EINP").await?;
    let deadline = Instant::now() + Duration::from_secs(secs);
    let mut video_name = String::from("?");
    let mut audio_name = String::from("?");
    let mut samples = Vec::new();
    let mut failed = offer.failed.clone();
    let mut offer = offer;
    while Instant::now() < deadline {
        let left = deadline.saturating_duration_since(Instant::now());
        let event = match tokio::time::timeout(left, events_rx.recv()).await {
            Ok(Some(event)) => event,
            _ => break,
        };
        match &event {
            PlayerEvent::Video(c) => {
                video_name = format!("{} ({}) {}x{} {} kbit/s target", c.video.map(Video::token).unwrap_or("?"), c.encoder, c.width, c.height, c.bitrate_kbps);
                println!("config video {video_name}{}", if c.reason.is_empty() { String::new() } else { format!(" reason: {}", c.reason) });
                if !c.host.video.is_empty() {
                    println!("host encoders: {:?}", c.host.video.iter().map(|e| format!("{}={}", e.video.token(), e.encoder)).collect::<Vec<_>>());
                    println!("host audio: {:?}", c.host.audio.iter().map(|a| a.token()).collect::<Vec<_>>());
                }
            }
            PlayerEvent::Audio(c) => {
                audio_name = format!("{} ({}) {} Hz {} ch", c.audio.map(Audio::token).unwrap_or("?"), c.encoder, c.sample_rate, c.channels);
                println!("config audio {audio_name}");
            }
            PlayerEvent::Stats(s) => {
                println!("stats {} {}x{} {:.1} fps {:.2} Mbit/s", s.video.map(Video::token).unwrap_or("?"), s.width, s.height, s.fps, s.video_mbps);
                samples.push((s.fps, s.video_mbps));
            }
            PlayerEvent::VideoFailed { video, reason } => {
                println!("decoder {} failed: {reason}", video.token());
                failed.push(*video);
                offer.failed = failed.clone();
                if let Some(send) = offer_send.as_mut() {
                    player::resend_offer(send, &offer).await?;
                }
            }
            PlayerEvent::AudioFailed { audio, reason } => println!("audio decoder {} failed: {reason}", audio.token()),
        }
    }
    let n = samples.len().max(1) as f64;
    let skip = if samples.len() > 2 { 1 } else { 0 };
    let tail = &samples[skip..];
    let tn = tail.len().max(1) as f64;
    println!(
        "summary negotiates={negotiates} video={video_name} audio={audio_name} frames={} audio_frames={} avg_fps={:.1} avg_mbps={:.2} (samples {})",
        frames.load(Ordering::Relaxed),
        audio_samples.load(Ordering::Relaxed),
        tail.iter().map(|s| s.0).sum::<f64>() / tn,
        tail.iter().map(|s| s.1).sum::<f64>() / tn,
        n
    );
    conn.close(0u32.into(), b"done");
    endpoint.wait_idle().await;
    Ok(())
}
