//! One connection carries a JPEG, a PCM chunk, a key, a pointer, and a gamepad.

use std::path::PathBuf;
use std::time::Duration;

use etherquic::{
    client_endpoint, install_pure_crypto, peer_to_socket_addr, server_endpoint, Fabric,
};
use etherquic_input::{Axes, Body, Buttons, Event, KeySource, PAD_SOUTH, PAD_UP};
use etherquic_play::{
    decode_jpeg, encode_jpeg, fit_rgba, play, serve, AudioChunk, CLIENT_MAC, HOST_MAC, SERVICE_ID,
};
use tokio::sync::mpsc;
use tokio::time::timeout;

#[tokio::test]
async fn picture_tone_key_pointer_and_pad_cross_the_fabric(
) -> Result<(), Box<dyn std::error::Error>> {
    install_pure_crypto();
    let rgba = {
        let mut px = vec![0u8; 4 * 4 * 4];
        px[0] = 200;
        px[1] = 20;
        px[2] = 20;
        px[3] = 255;
        px
    };
    let picture = fit_rgba(&rgba, 4, 4, 1280)?;
    let jpeg = encode_jpeg(&picture, 80)?;
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

    let cert = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../certs/cert.pem");
    let key_pem = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../certs/key.pem");
    let fabric = Fabric::new(1500);
    let server = server_endpoint(fabric.clone(), HOST_MAC, SERVICE_ID, &cert, &key_pem)?;
    let client = client_endpoint(fabric, CLIENT_MAC, SERVICE_ID)?;
    let server_addr = peer_to_socket_addr(HOST_MAC, SERVICE_ID);

    let (video_tx, video_rx) = mpsc::channel(1);
    let (audio_tx, audio_rx) = mpsc::channel(1);
    let (input_tx, mut input_rx) = mpsc::channel(1);
    let (got_video_tx, mut got_video_rx) = mpsc::channel(1);
    let (got_audio_tx, mut got_audio_rx) = mpsc::channel(1);
    let (player_tx, player_rx) = mpsc::channel(1);

    let host = tokio::spawn(async move {
        let incoming = server.accept().await.ok_or("no player")?;
        let conn = incoming.await?;
        serve(conn, video_rx, audio_rx, input_tx, None).await?;
        Ok::<(), Box<dyn std::error::Error + Send + Sync>>(())
    });
    let player = tokio::spawn(async move {
        let conn = client.connect(server_addr, "localhost")?.await?;
        play(conn, got_video_tx, got_audio_tx, player_rx, None).await?;
        Ok::<(), Box<dyn std::error::Error + Send + Sync>>(())
    });

    video_tx.send(jpeg.clone()).await?;
    audio_tx.send(tone.clone()).await?;
    player_tx
        .send(vec![key.clone(), pointer.clone(), pad.clone()])
        .await?;

    let got_events = timeout(Duration::from_secs(10), input_rx.recv())
        .await?
        .ok_or("host got no input")?;
    let got_jpeg = timeout(Duration::from_secs(10), got_video_rx.recv())
        .await?
        .ok_or("player got no picture")?;
    let got_tone = timeout(Duration::from_secs(10), got_audio_rx.recv())
        .await?
        .ok_or("player got no audio")?;
    drop(video_tx);
    drop(audio_tx);
    drop(player_tx);

    assert_eq!(got_events, vec![key, pointer, pad]);
    assert_eq!(got_tone.sample_rate, tone.sample_rate);
    assert_eq!(got_tone.channels, tone.channels);
    assert_eq!(got_tone.pcm, tone.pcm);
    assert_eq!(got_jpeg, jpeg);
    let shown = decode_jpeg(&got_jpeg)?;
    assert_eq!(shown.width, picture.width);
    assert_eq!(shown.height, picture.height);

    timeout(Duration::from_secs(10), host)
        .await??
        .map_err(|error| error.to_string())?;
    timeout(Duration::from_secs(10), player)
        .await??
        .map_err(|error| error.to_string())?;
    Ok(())
}
