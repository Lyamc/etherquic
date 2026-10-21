//! The desktop entries, not a hand-built pipe pair.
//! `host_desktop` and `connect_view` are what the window and etherquic-play call.

#![cfg(feature = "desktop")]

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use etherquic::L2Net;
use etherquic_play::{
    connect_view, host_desktop, keystroke_events, queue_chat, queue_clipboard, queue_file,
    queue_keystroke_string, set_desk_roster, set_input_recorder, stderr_status, take_host_mail,
    take_player_mail, Audience, AudioId, ChatMessage, CodecChoice, ConnectSession, DeskMail,
    HostSession, NoteSide, Participant, PicturePref, CLIENT_MAC, HOST_MAC, SERVICE_ID,
};

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

fn queue_until(mut send: impl FnMut() -> Result<(), String>) {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        match send() {
            Ok(()) => return,
            Err(error) if error == "session notes are closed" && Instant::now() < deadline => {
                thread::sleep(Duration::from_millis(40));
            }
            Err(error) => panic!("queue failed: {error}"),
        }
    }
}

struct Flags {
    host: Arc<AtomicBool>,
    player: Arc<AtomicBool>,
}

impl Drop for Flags {
    fn drop(&mut self) {
        self.player.store(true, Ordering::SeqCst);
        self.host.store(true, Ordering::SeqCst);
        set_input_recorder(None);
    }
}

#[test]
fn host_desktop_and_connect_view_carry_file_clipboard_keys_and_chat() {
    let refused = queue_file(NoteSide::Player, "", b"nope");
    assert_eq!(refused.unwrap_err(), "file name is empty");

    set_desk_roster(room());
    let _ = take_host_mail();
    let _ = take_player_mail();
    let recorder = Arc::new(Mutex::new(Vec::new()));
    set_input_recorder(Some(Arc::clone(&recorder)));

    let net = L2Net::memory();
    let stop_host = Arc::new(AtomicBool::new(false));
    let stop_player = Arc::new(AtomicBool::new(false));
    let flags = Flags {
        host: Arc::clone(&stop_host),
        player: Arc::clone(&stop_player),
    };
    let up = Arc::new(AtomicBool::new(false));
    let peers = Arc::new(AtomicUsize::new(0));
    let certs = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("certs");

    let host_net = Arc::clone(&net);
    let host_stop = Arc::clone(&stop_host);
    let host_up = Arc::clone(&up);
    let host_peers = Arc::clone(&peers);
    let host = thread::spawn(move || {
        host_desktop(
            host_net,
            HostSession {
                local_mac: HOST_MAC,
                service_id: SERVICE_ID,
                max_width: 320,
                fps: etherquic::negotiate::FpsWish::Fixed(5),
                cert: certs.join("cert.pem"),
                key: certs.join("key.pem"),
                audio_device: None,
            },
            host_stop,
            host_up,
            host_peers,
            stderr_status(),
        )
    });

    let deadline = Instant::now() + Duration::from_secs(30);
    while !up.load(Ordering::SeqCst) {
        if host.is_finished() {
            let error = host.join().expect("host thread");
            panic!("host ended before it was listening: {error:?}");
        }
        if Instant::now() > deadline {
            panic!("host did not start listening");
        }
        thread::sleep(Duration::from_millis(40));
    }

    let player_net = net;
    let player_stop = Arc::clone(&stop_player);
    let player = thread::spawn(move || {
        connect_view(
            Some(player_net),
            ConnectSession {
                local_mac: CLIENT_MAC,
                peer_mac: HOST_MAC,
                service_id: SERVICE_ID,
                trace_input: false,
                udp: None,
                l2_offer: false,
                rtp: None,
                view: etherquic_play::live(etherquic_play::ViewOptions::default()),
                tune: etherquic_play::live_tune(etherquic_play::SessionTune::default()),
                codecs: CodecChoice {
                    video: vec![PicturePref::Mjpeg],
                    audio: vec![AudioId::Pcm],
                    video_pin: Some(PicturePref::Mjpeg),
                    audio_pin: Some(AudioId::Pcm),
                    max_width: 320,
                    max_height: 0,
                    bitrate_kbps: 0,
                    quality: 0,
                    dirty_rects: false,
                    yuv: etherquic::negotiate::YuvFormat::YUV444P8,
                    fps: etherquic::negotiate::FpsWish::Auto,
                    audio_bitrate_kbps: 0,
                    audio_channels: 0,
                    view_only: false,
                    exclusive: false,
                    carrier: None,
                },
                picture: Arc::new(AtomicU64::new(0)),
            },
            player_stop,
            stderr_status(),
        )
    });

    let connected = Instant::now() + Duration::from_secs(60);
    while peers.load(Ordering::SeqCst) == 0 {
        if player.is_finished() {
            let error = player.join().expect("player thread");
            panic!("player ended before the host saw it: {error:?}");
        }
        if Instant::now() > connected {
            panic!("player did not reach the host");
        }
        thread::sleep(Duration::from_millis(40));
    }

    queue_until(|| queue_file(NoteSide::Host, "from-host.bin", b"host-one"));
    queue_until(|| queue_file(NoteSide::Host, "second.bin", b"host-two"));
    queue_until(|| queue_file(NoteSide::Player, "from-player.bin", b"player-bytes"));
    queue_until(|| queue_clipboard(NoteSide::Player, "paste this exact clipboard"));
    queue_keystroke_string("ab");
    queue_until(|| {
        queue_chat(
            NoteSide::Player,
            ChatMessage {
                from: "ada".into(),
                audience: Audience::All,
                text: "hello all".into(),
            },
        )
    });
    queue_until(|| {
        queue_chat(
            NoteSide::Player,
            ChatMessage {
                from: "ada".into(),
                audience: Audience::Team("red".into()),
                text: "red only".into(),
            },
        )
    });
    queue_until(|| {
        queue_chat(
            NoteSide::Player,
            ChatMessage {
                from: "ada".into(),
                audience: Audience::One("cy".into()),
                text: "psst".into(),
            },
        )
    });

    let mut host_mail = DeskMail::default();
    let mut player_mail = DeskMail::default();
    let mut keys = Vec::new();
    let got = Instant::now() + Duration::from_secs(60);
    loop {
        let next = take_host_mail();
        host_mail.files.extend(next.files);
        host_mail.clipboard.extend(next.clipboard);
        host_mail.chat.extend(next.chat);
        let next = take_player_mail();
        player_mail.files.extend(next.files);
        if let Ok(mut guard) = recorder.lock() {
            keys.extend(guard.drain(..));
        }
        let files_ok = player_mail.files.len() >= 2 && host_mail.files.len() >= 1;
        let clip_ok = host_mail
            .clipboard
            .iter()
            .any(|text| text == "paste this exact clipboard");
        let keys_ok = keys.len() >= 4;
        let chat_ok = host_mail.chat.len() >= 3;
        if files_ok && clip_ok && keys_ok && chat_ok {
            break;
        }
        if Instant::now() > got {
            panic!(
                "desktop session did not deliver the notes\nfiles host {:?} player {:?}\nclip {:?}\nkeys {}\nchat {:?}",
                host_mail.files.iter().map(|file| &file.name).collect::<Vec<_>>(),
                player_mail.files.iter().map(|file| &file.name).collect::<Vec<_>>(),
                host_mail.clipboard,
                keys.len(),
                host_mail.chat.iter().map(|routed| &routed.message.text).collect::<Vec<_>>()
            );
        }
        thread::sleep(Duration::from_millis(50));
    }

    let host_file = host_mail
        .files
        .iter()
        .find(|file| file.name == "from-player.bin")
        .expect("player file");
    assert_eq!(host_file.bytes, b"player-bytes");
    let first = player_mail
        .files
        .iter()
        .find(|file| file.name == "from-host.bin")
        .expect("host file");
    assert_eq!(first.bytes, b"host-one");
    let second = player_mail
        .files
        .iter()
        .find(|file| file.name == "second.bin")
        .expect("second file");
    assert_eq!(second.bytes, b"host-two");
    assert_eq!(
        host_mail
            .clipboard
            .iter()
            .find(|text| text.as_str() == "paste this exact clipboard")
            .map(String::as_str),
        Some("paste this exact clipboard")
    );
    let expected = keystroke_events("ab");
    assert!(
        keys.windows(expected.len())
            .any(|window| window == expected.as_slice()),
        "host did not receive the key down/up sequence"
    );
    let all = host_mail
        .chat
        .iter()
        .find(|routed| routed.message.text == "hello all")
        .expect("all");
    let team = host_mail
        .chat
        .iter()
        .find(|routed| routed.message.text == "red only")
        .expect("team");
    let one = host_mail
        .chat
        .iter()
        .find(|routed| routed.message.text == "psst")
        .expect("one");
    assert_eq!(all.recipients, vec!["ada", "bea", "cy"]);
    assert_eq!(team.recipients, vec!["ada", "bea"]);
    assert_eq!(one.recipients, vec!["cy"]);
    assert_ne!(all.recipients, team.recipients);
    assert_ne!(all.recipients, one.recipients);
    assert_ne!(team.recipients, one.recipients);

    drop(flags);
    let _ = player.join();
    let _ = host.join();
}
