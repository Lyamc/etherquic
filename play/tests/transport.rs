//! One play session on a real UDP socket, IPv4 and IPv6.
//!
//! The in-memory fabric test is a different path. This one opens the shipped
//! UDP Quinn endpoints, then calls the same `serve` and `play` the host and
//! the player call.

use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use etherquic::{udp_client_endpoint_on, udp_server_endpoint, L2Net, ETHERTYPE_QUIC};
use etherquic_input::{Axes, Body, Buttons, Event, KeySource, PAD_SOUTH, PAD_UP};
use etherquic_play::etherquic_media::host::FrameSlot;
use etherquic_play::etherquic_media::negotiate::{Audio, Limit, Offer, Video};
use etherquic_play::etherquic_media::{Frame, Sink};
use etherquic_play::{fit_rgba, play, serve, AudioChunk, HostMedia, PlayerMedia};
use quinn::Endpoint;
use tokio::sync::mpsc;
use tokio::time::timeout;

#[tokio::test]
async fn picture_tone_key_pointer_and_pad_cross_udp_v4_and_v6(
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    match L2Net::open(None, 1500, ETHERTYPE_QUIC) {
        Ok(net) => {
            println!(
                "play-transport ethernet opened ethertype=0x{ETHERTYPE_QUIC:04x} but this process has no second L2 peer; confirming on udp"
            );
            drop(net);
        }
        Err(error) => {
            println!("play-transport ethernet error: {error}");
        }
    }

    let v4 = SocketAddr::from((Ipv4Addr::LOCALHOST, 0));
    println!("{}", udp_session(v4).await?);
    let v6 = SocketAddr::from((Ipv6Addr::LOCALHOST, 0));
    match udp_session(v6).await {
        Ok(line) => println!("{line}"),
        Err(error) if bind_failure(&error.to_string()) => {
            println!("play-transport udp/ipv6 bind error: {error}");
        }
        Err(error) => return Err(error),
    }
    Ok(())
}

async fn udp_session(bind: SocketAddr) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
    let cert = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../certs/cert.pem");
    let key_pem = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../certs/key.pem");
    let server = udp_server_endpoint(bind, &cert, &key_pem)
        .map_err(|error| format!("bind server {bind}: {error}"))?;
    let client =
        udp_client_endpoint_on(bind).map_err(|error| format!("bind client {bind}: {error}"))?;
    let server_addr = server
        .local_addr()
        .map_err(|error| format!("server address: {error}"))?;
    let client_addr = client
        .local_addr()
        .map_err(|error| format!("client address: {error}"))?;
    if bind.is_ipv4() {
        if !server_addr.is_ipv4() || !client_addr.is_ipv4() {
            return Err(format!("ipv4 sockets bound {server_addr} and {client_addr}").into());
        }
    } else if !server_addr.is_ipv6() || !client_addr.is_ipv6() {
        return Err(format!("ipv6 sockets bound {server_addr} and {client_addr}").into());
    }
    if server_addr.port() == 0 || client_addr.port() == 0 {
        return Err(format!("unbound sockets {server_addr} {client_addr}").into());
    }
    let family = if server_addr.is_ipv4() {
        "ipv4"
    } else {
        "ipv6"
    };
    let summary = exchange(server, client, server_addr).await?;
    Ok(format!(
        "play-transport udp/{family} server={server_addr} client={client_addr} {summary} assertions=passed"
    ))
}

async fn exchange(
    server: Endpoint,
    client: Endpoint,
    server_addr: SocketAddr,
) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
    let rgba = {
        let mut px = vec![0u8; 4 * 4 * 4];
        px[0] = 200;
        px[1] = 20;
        px[2] = 20;
        px[3] = 255;
        px
    };
    let picture = fit_rgba(&rgba, 4, 4, 1280)?;
    let frame = Frame {
        width: picture.width,
        height: picture.height,
        rgb: picture.rgb,
    };
    let tone = AudioChunk {
        sample_rate: 8_000,
        channels: 1,
        pcm: vec![0, 8000, 0, -8000],
    };
    let key = Event {
        time_us: 7,
        body: Body::Key {
            down: true,
            source: KeySource::Keysym,
            code: u32::from(b'a'),
        },
    };
    let pointer = Event {
        time_us: 8,
        body: Body::Pointer {
            x: 12_000,
            y: 34_000,
            buttons: Buttons::LEFT | Buttons::RIGHT,
            wheel_y: 3,
            wheel_x: -1,
        },
    };
    let axes = Axes {
        lx: 100,
        ly: 50,
        rx: -10,
        ry: 20,
        lt: 16_383,
        rt: 32_767,
    };
    let pad = Event {
        time_us: 9,
        body: Body::Gamepad {
            index: 1,
            buttons: PAD_SOUTH | PAD_UP,
            axes,
        },
    };

    let frames = FrameSlot::new();
    let host_frames = frames.clone();
    let (audio_tx, audio_rx) = mpsc::channel(1);
    let (input_tx, mut input_rx) = mpsc::channel(1);
    let (player_tx, player_rx) = mpsc::channel(1);
    let (heard_tx, mut heard_rx) = mpsc::channel(1);
    let (shown_tx, shown_rx) = std::sync::mpsc::channel();

    let host = tokio::spawn(async move {
        let incoming = server.accept().await.ok_or("no player")?;
        let conn = incoming.await?;
        let media = HostMedia {
            frames: host_frames,
            audio: audio_rx,
            fps: 10,
            log: Arc::new(|_: &str| {}),
            cursor: tokio::sync::watch::channel(None).1,
        };
        serve(conn, media, input_tx, None).await?;
        Ok::<(), Box<dyn std::error::Error + Send + Sync>>(())
    });
    let player = tokio::spawn(async move {
        let conn = client.connect(server_addr, "localhost")?.await?;
        let sink: Sink<Frame> = Arc::new(move |frame| shown_tx.send(frame).is_ok());
        let (events, keep) = mpsc::unbounded_channel();
        // The session reports codec events here. This test only checks the picture.
        std::mem::forget(keep);
        let media = PlayerMedia {
            offer: Offer {
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
            },
            frames: sink,
            events,
            live: tokio::sync::watch::channel(etherquic_play::LiveOffer {
                look: etherquic::look::Look::Off,
                quality: 0,
            })
            .1,
        };
        play(conn, media, heard_tx, player_rx, None).await?;
        Ok::<(), Box<dyn std::error::Error + Send + Sync>>(())
    });

    frames.put(frame.clone());
    audio_tx.send(tone.clone()).await?;
    player_tx
        .send(vec![key.clone(), pointer.clone(), pad.clone()])
        .await?;

    let got_events = timeout(Duration::from_secs(10), input_rx.recv())
        .await?
        .ok_or("host got no input")?;
    let shown = timeout(
        Duration::from_secs(10),
        tokio::task::spawn_blocking(move || shown_rx.recv()),
    )
    .await?
    .map_err(|error| error.to_string())?
    .map_err(|_| "player got no picture")?;
    let got_tone = timeout(Duration::from_secs(10), heard_rx.recv())
        .await?
        .ok_or("player got no audio")?;
    frames.close();
    drop(audio_tx);
    drop(player_tx);

    assert_eq!(got_events, vec![key, pointer, pad]);
    assert_eq!(got_tone.sample_rate, tone.sample_rate);
    assert_eq!(got_tone.channels, tone.channels);
    assert_eq!(got_tone.pcm, tone.pcm);
    assert_eq!(shown.width, frame.width);
    assert_eq!(shown.height, frame.height);
    assert!(shown.rgb[0] > shown.rgb[1] && shown.rgb[0] > shown.rgb[2]);

    timeout(Duration::from_secs(10), host)
        .await??
        .map_err(|error| error.to_string())?;
    timeout(Duration::from_secs(10), player)
        .await??
        .map_err(|error| error.to_string())?;

    let (x, y, buttons, wheel_y, wheel_x) = match &got_events[1].body {
        Body::Pointer {
            x,
            y,
            buttons,
            wheel_y,
            wheel_x,
        } => (*x, *y, *buttons, *wheel_y, *wheel_x),
        _ => return Err("received input is missing the pointer".into()),
    };
    let (index, pad_buttons) = match &got_events[2].body {
        Body::Gamepad { index, buttons, .. } => (*index, *buttons),
        _ => return Err("received input is missing the gamepad".into()),
    };
    let key_code = match got_events[0].body {
        Body::Key { code, .. } => code,
        _ => return Err("received input is missing the key".into()),
    };
    Ok(format!(
        "picture={}x{} pcm={}/{}/{} key=0x{key_code:x} pointer={x},{y},buttons={buttons:#x},wheel={wheel_y},{wheel_x} gamepad=index{index},buttons={pad_buttons:#x}",
        shown.width,
        shown.height,
        got_tone.sample_rate,
        got_tone.channels,
        got_tone.pcm.len(),
    ))
}

fn bind_failure(text: &str) -> bool {
    let text = text.to_ascii_lowercase();
    text.contains("address family")
        || text.contains("cannot assign requested address")
        || text.contains("network is unreachable")
        || text.contains("address not available")
}
