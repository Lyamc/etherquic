//! Phone path: parse the host-card text and join over UDP.
//!
//! This is what the Android player runs. Ethernet is not opened.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use etherquic::{install_pure_crypto, udp_server_endpoint};
use etherquic_play::etherquic_media::host::FrameSlot;
use etherquic_play::etherquic_media::negotiate::{Audio, Limit, Offer, Video};
use etherquic_play::etherquic_media::{Frame, Sink};
use etherquic_play::{join_udp, parse_share, serve, udp_peer, AudioChunk, HostMedia, PlayerMedia};
use tokio::sync::mpsc;
use tokio::time::timeout;

fn mjpeg_offer() -> Offer {
    Offer {
        version: 1,
        video: vec![Video::Mjpeg],
        audio: vec![Audio::Pcm],
        max: Limit::NONE,
        bitrate_kbps: 0,
        failed: Vec::new(),
        knows_pyrowave_cpu: true,
        knows_dirty_rects: true,
        look: etherquic::look::Look::Off,
        quality: 0,
    }
}

fn red_frame() -> Frame {
    let mut rgb = vec![0u8; 4 * 4 * 3];
    rgb[0] = 220;
    rgb[1] = 10;
    rgb[2] = 10;
    Frame {
        width: 4,
        height: 4,
        rgb,
    }
}

fn host_media(frames: FrameSlot, audio: mpsc::Receiver<AudioChunk>) -> HostMedia {
    HostMedia {
        frames,
        audio,
        fps: 10,
        log: Arc::new(|_: &str| {}),
        cursor: tokio::sync::watch::channel(None).1,
    }
}

fn player_media(offer: Offer, shown: std::sync::mpsc::Sender<Frame>) -> PlayerMedia {
    let sink: Sink<Frame> = Arc::new(move |frame| shown.send(frame).is_ok());
    let (events, keep) = mpsc::unbounded_channel();
    std::mem::forget(keep);
    PlayerMedia {
        offer,
        frames: sink,
        events,
        live: tokio::sync::watch::channel(etherquic_play::LiveOffer {
            look: etherquic::look::Look::Off,
            quality: 0,
        })
        .1,
    }
}

fn certs() -> (PathBuf, PathBuf) {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../certs");
    (root.join("cert.pem"), root.join("key.pem"))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn share_card_udp_join_shows_a_picture() -> Result<(), Box<dyn std::error::Error>> {
    install_pure_crypto();
    let (cert, key) = certs();
    let server = udp_server_endpoint("127.0.0.1:0".parse()?, &cert, &key)?;
    let peer = server.local_addr()?;
    let text = format!(
        "etherquic\nmac 02:00:00:00:00:01\nservice 4433\nport {}\nip 127.0.0.1\n",
        peer.port()
    );
    let share = parse_share(&text)?;
    assert_eq!(udp_peer(&share)?, peer);

    let frames = FrameSlot::new();
    let host_frames = frames.clone();
    let (audio_tx, audio_rx) = mpsc::channel(4);
    let (host_input_tx, mut host_input) = mpsc::channel(4);
    let host = tokio::spawn(async move {
        let incoming = server.accept().await.ok_or("no player")?;
        let conn = incoming.await.map_err(|error| error.to_string())?;
        serve(conn, host_media(host_frames, audio_rx), host_input_tx, None)
            .await
            .map_err(|error| error.to_string())
    });

    let (shown_tx, shown) = std::sync::mpsc::channel();
    let (heard_tx, mut heard) = mpsc::channel(4);
    let (_player_input, player_input_rx) = mpsc::channel(4);
    let stop = Arc::new(AtomicBool::new(false));
    let join_stop = Arc::clone(&stop);
    let player = tokio::spawn(async move {
        join_udp(
            peer,
            player_media(mjpeg_offer(), shown_tx),
            heard_tx,
            player_input_rx,
            join_stop,
        )
        .await
        .map_err(|error| error.to_string())
    });

    let frame = red_frame();
    frames.put(frame.clone());
    let tone = AudioChunk {
        sample_rate: 8_000,
        channels: 1,
        pcm: vec![0, 8000, 0, -8000],
    };
    audio_tx.send(tone.clone()).await?;

    let shown_rx = shown;
    let got = timeout(
        Duration::from_secs(30),
        tokio::task::spawn_blocking(move || shown_rx.recv()),
    )
    .await?
    .map_err(|error| error.to_string())?
    .map_err(|_| "player got no picture")?;
    assert_eq!(got.width, frame.width);
    assert_eq!(got.height, frame.height);
    assert!(got.rgb[0] > got.rgb[1] && got.rgb[0] > got.rgb[2]);

    let got_tone = timeout(Duration::from_secs(30), heard.recv())
        .await?
        .ok_or("player got no audio")?;
    assert_eq!(got_tone.pcm, tone.pcm);

    frames.close();
    drop(audio_tx);
    stop.store(true, Ordering::SeqCst);
    let _ = timeout(Duration::from_secs(5), player).await;
    let _ = timeout(Duration::from_secs(5), host).await;
    let _ = host_input.recv();
    Ok(())
}

#[test]
fn join_does_not_open_an_ethernet_socket() {
    // udp_client_endpoint_on is the Android bind. A missing adapter must not
    // be required to pick the peer from a card.
    let text = "etherquic\nmac 02:00:00:00:00:01\nservice 4433\nport 9\nip 10.0.0.2\n";
    let share = parse_share(text).expect("share");
    let peer = udp_peer(&share).expect("udp");
    assert_eq!(peer, "10.0.0.2:9".parse::<SocketAddr>().unwrap());
}
