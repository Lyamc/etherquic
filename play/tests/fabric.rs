//! Play sessions on the in-memory fabric.
//!
//! Picture, audio, and input use `serve` / `play`. File transfer, clipboard
//! text, keystroke strings, and chat use `serve_session` / `play_session`,
//! which are that same session with the extra streams.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use etherquic::{
    client_endpoint, install_pure_crypto, peer_to_socket_addr, server_endpoint, Fabric,
};
use etherquic_input::{Axes, Body, Buttons, Event, KeySource, PAD_SOUTH, PAD_UP};
use etherquic_play::etherquic_media::host::FrameSlot;
use etherquic_play::etherquic_media::negotiate::{Audio, Limit, Offer, Video};
use etherquic_play::etherquic_media::{Frame, Sink};
use etherquic_play::{
    keystroke_events, play, play_session, serve, serve_session, submit_chat, submit_clipboard,
    submit_file, submit_keystrokes, Audience, AudioChunk, ChatMessage, HostMedia, NamedFile,
    Participant, Pipes, PlayerMedia, CLIENT_MAC, HOST_MAC, SERVICE_ID,
};
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

fn endpoints() -> (quinn::Endpoint, quinn::Endpoint, std::net::SocketAddr) {
    let cert = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../certs/cert.pem");
    let key_pem = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../certs/key.pem");
    let fabric = Fabric::new(1500);
    let server = server_endpoint(fabric.clone(), HOST_MAC, SERVICE_ID, &cert, &key_pem)
        .expect("server endpoint");
    let client = client_endpoint(fabric, CLIENT_MAC, SERVICE_ID).expect("client endpoint");
    let server_addr = peer_to_socket_addr(HOST_MAC, SERVICE_ID);
    (server, client, server_addr)
}

fn sample_events() -> (Event, Event, Event) {
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
    (key, pointer, pad)
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

struct Desk {
    frames: FrameSlot,
    audio_tx: mpsc::Sender<AudioChunk>,
    player_input: mpsc::Sender<Vec<Event>>,
    host_input: mpsc::Receiver<Vec<Event>>,
    shown: std::sync::mpsc::Receiver<Frame>,
    heard: mpsc::Receiver<AudioChunk>,
    host: tokio::task::JoinHandle<Result<(), String>>,
    player: tokio::task::JoinHandle<Result<(), String>>,
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
    let (events, _keep) = mpsc::unbounded_channel();
    // Leak the receiver so a full buffer cannot stop the session. It is
    // unbounded, and the test process ends with the session.
    std::mem::forget(_keep);
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

async fn spawn_desk(pipes: Option<(Pipes, Pipes)>) -> Result<Desk, Box<dyn std::error::Error>> {
    let (server, client, server_addr) = endpoints();
    let frames = FrameSlot::new();
    let host_frames = frames.clone();
    let (audio_tx, audio_rx) = mpsc::channel(4);
    let (player_input, player_input_rx) = mpsc::channel(4);
    let (host_input_tx, host_input) = mpsc::channel(4);
    let (shown_tx, shown) = std::sync::mpsc::channel();
    let (heard_tx, heard) = mpsc::channel(4);
    let offer = mjpeg_offer();
    let (host_pipes, player_pipes) = match pipes {
        Some((host, player)) => (Some(host), Some(player)),
        None => (None, None),
    };

    let host = tokio::spawn(async move {
        let incoming = server.accept().await.ok_or("no player")?;
        let conn = incoming.await.map_err(|error| error.to_string())?;
        let media = host_media(host_frames, audio_rx);
        let result = if let Some(pipes) = host_pipes {
            serve_session(conn, media, host_input_tx, None, pipes).await
        } else {
            serve(conn, media, host_input_tx, None).await
        };
        result.map_err(|error| error.to_string())
    });
    let player = tokio::spawn(async move {
        let connecting = client
            .connect(server_addr, "localhost")
            .map_err(|error| error.to_string())?;
        let conn = connecting.await.map_err(|error| error.to_string())?;
        let media = player_media(offer, shown_tx);
        let result = if let Some(pipes) = player_pipes {
            play_session(conn, media, heard_tx, player_input_rx, None, pipes).await
        } else {
            play(conn, media, heard_tx, player_input_rx, None).await
        };
        result.map_err(|error| error.to_string())
    });
    Ok(Desk {
        frames,
        audio_tx,
        player_input,
        host_input,
        shown,
        heard,
        host,
        player,
    })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn picture_tone_key_pointer_and_pad_cross_the_fabric(
) -> Result<(), Box<dyn std::error::Error>> {
    install_pure_crypto();
    let mut desk = spawn_desk(None).await?;
    let frame = red_frame();
    desk.frames.put(frame.clone());
    let tone = AudioChunk {
        sample_rate: 8_000,
        channels: 1,
        pcm: vec![0, 8000, 0, -8000],
    };
    desk.audio_tx.send(tone.clone()).await?;
    let (key, pointer, pad) = sample_events();
    desk.player_input
        .send(vec![key.clone(), pointer.clone(), pad.clone()])
        .await?;

    let got_events = timeout(Duration::from_secs(120), desk.host_input.recv())
        .await?
        .ok_or("host got no input")?;
    let got_tone = timeout(Duration::from_secs(30), desk.heard.recv())
        .await?
        .ok_or("player got no audio")?;
    let shown_rx = desk.shown;
    let shown = timeout(
        Duration::from_secs(30),
        tokio::task::spawn_blocking(move || shown_rx.recv()),
    )
    .await?
    .map_err(|error| error.to_string())?
    .map_err(|_| "player got no picture")?;

    assert_eq!(got_events, vec![key, pointer, pad]);
    assert_eq!(got_tone.sample_rate, tone.sample_rate);
    assert_eq!(got_tone.channels, tone.channels);
    assert_eq!(got_tone.pcm, tone.pcm);
    assert_eq!(shown.width, frame.width);
    assert_eq!(shown.height, frame.height);
    assert!(shown.rgb[0] > shown.rgb[1] && shown.rgb[0] > shown.rgb[2]);

    desk.frames.close();
    drop(desk.audio_tx);
    drop(desk.player_input);
    timeout(Duration::from_secs(30), desk.host)
        .await??
        .map_err(|error| error)?;
    timeout(Duration::from_secs(30), desk.player)
        .await??
        .map_err(|error| error)?;
    Ok(())
}

fn room() -> Vec<Participant> {
    vec![
        Participant {
            id: "ada".into(),
            team: "red".into(),
        },
        Participant {
            id: "bea".into(),
            team: "red".into(),
        },
        Participant {
            id: "cy".into(),
            team: "blue".into(),
        },
    ]
}

fn closed_sender<T>() -> mpsc::Sender<T> {
    let (tx, _rx) = mpsc::channel(1);
    tx
}

fn closed_receiver<T>() -> mpsc::Receiver<T> {
    let (_tx, rx) = mpsc::channel(1);
    rx
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn file_clipboard_keystrokes_and_chat_cross_the_fabric(
) -> Result<(), Box<dyn std::error::Error>> {
    install_pure_crypto();
    let roster = room();
    let (host_files_tx, host_files_rx) = mpsc::channel(4);
    let (player_files_tx, player_files_rx) = mpsc::channel(4);
    let (host_got_tx, mut host_got_rx) = mpsc::channel(4);
    let (player_got_tx, mut player_got_rx) = mpsc::channel(4);
    let (clip_tx, clip_rx) = mpsc::channel(4);
    let (clip_got_tx, mut clip_got_rx) = mpsc::channel(4);
    let (chat_tx, chat_rx) = mpsc::channel(4);
    let (routed_tx, mut routed_rx) = mpsc::channel(4);

    let host_pipes = Pipes {
        files_out: host_files_rx,
        files_in: host_got_tx,
        clipboard_out: closed_receiver(),
        clipboard_in: clip_got_tx,
        chat_out: closed_receiver(),
        chat_in: routed_tx,
        roster: roster.clone(),
    };
    let player_pipes = Pipes {
        files_out: player_files_rx,
        files_in: player_got_tx,
        clipboard_out: clip_rx,
        clipboard_in: closed_sender(),
        chat_out: chat_rx,
        chat_in: closed_sender(),
        roster,
    };

    let mut desk = spawn_desk(Some((host_pipes, player_pipes))).await?;

    let refused = submit_file(&host_files_tx, "", b"nope").await;
    assert_eq!(refused.unwrap_err(), "file name is empty");

    let first = b"host-one".to_vec();
    let second = b"host-two".to_vec();
    let back = b"player-bytes".to_vec();
    submit_file(&host_files_tx, "from-host.bin", &first).await?;
    submit_file(&host_files_tx, "second.bin", &second).await?;
    submit_file(&player_files_tx, "from-player.bin", &back).await?;

    let clip = "paste this exact clipboard";
    submit_clipboard(&clip_tx, clip).await?;
    submit_keystrokes(&desk.player_input, "ab").await?;

    let all_text = "hello all";
    let team_text = "red only";
    let one_text = "psst";
    submit_chat(
        &chat_tx,
        ChatMessage {
            from: "ada".into(),
            audience: Audience::All,
            text: all_text.into(),
        },
    )
    .await?;
    submit_chat(
        &chat_tx,
        ChatMessage {
            from: "ada".into(),
            audience: Audience::Team("red".into()),
            text: team_text.into(),
        },
    )
    .await?;
    submit_chat(
        &chat_tx,
        ChatMessage {
            from: "ada".into(),
            audience: Audience::One("cy".into()),
            text: one_text.into(),
        },
    )
    .await?;

    let wait = Duration::from_secs(120);
    let got_first = timeout(wait, player_got_rx.recv())
        .await?
        .ok_or("player got no file")?;
    let got_second = timeout(Duration::from_secs(30), player_got_rx.recv())
        .await?
        .ok_or("player got no second file")?;
    let got_back = timeout(Duration::from_secs(30), host_got_rx.recv())
        .await?
        .ok_or("host got no file")?;
    let got_clip = timeout(Duration::from_secs(30), clip_got_rx.recv())
        .await?
        .ok_or("host got no clipboard")?;
    let got_keys = timeout(Duration::from_secs(30), desk.host_input.recv())
        .await?
        .ok_or("host got no keystrokes")?;
    let got_all = timeout(Duration::from_secs(30), routed_rx.recv())
        .await?
        .ok_or("host got no all chat")?;
    let got_team = timeout(Duration::from_secs(30), routed_rx.recv())
        .await?
        .ok_or("host got no team chat")?;
    let got_one = timeout(Duration::from_secs(30), routed_rx.recv())
        .await?
        .ok_or("host got no direct chat")?;

    assert_eq!(
        got_first,
        NamedFile {
            name: "from-host.bin".into(),
            bytes: first,
        }
    );
    assert_eq!(
        got_second,
        NamedFile {
            name: "second.bin".into(),
            bytes: second,
        }
    );
    assert_eq!(
        got_back,
        NamedFile {
            name: "from-player.bin".into(),
            bytes: back,
        }
    );
    assert_ne!(got_first.name, "");
    assert_eq!(got_clip, clip);
    assert_eq!(got_keys, keystroke_events("ab"));

    assert_eq!(got_all.message.text, all_text);
    assert_eq!(got_team.message.text, team_text);
    assert_eq!(got_one.message.text, one_text);
    assert_eq!(got_all.recipients, vec!["ada", "bea", "cy"]);
    assert_eq!(got_team.recipients, vec!["ada", "bea"]);
    assert_eq!(got_one.recipients, vec!["cy"]);
    assert_ne!(got_all.recipients, got_team.recipients);
    assert_ne!(got_all.recipients, got_one.recipients);
    assert_ne!(got_team.recipients, got_one.recipients);

    desk.frames.close();
    drop(desk.audio_tx);
    drop(desk.player_input);
    drop(host_files_tx);
    drop(player_files_tx);
    drop(clip_tx);
    drop(chat_tx);
    timeout(Duration::from_secs(30), desk.host)
        .await??
        .map_err(|error| error)?;
    timeout(Duration::from_secs(30), desk.player)
        .await??
        .map_err(|error| error)?;
    Ok(())
}
