//! Fresh consumer of the QUIC carriers. This is not the unit that implements them.

use std::path::PathBuf;

use etherquic::carrier::{exchange_picture, QUIC_DATAGRAMS, QUIC_STREAMS};

fn certs() -> (PathBuf, PathBuf) {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..").join("certs");
    (root.join("cert.pem"), root.join("key.pem"))
}

#[tokio::test]
async fn mode_1_sends_no_datagrams_and_mode_2_waits_for_carrier_2() {
    let (cert, key) = certs();
    let record = vec![9u8; 4_000];
    let mode1 = exchange_picture(&cert, &key, QUIC_STREAMS, true, &record)
        .await
        .expect("mode 1");
    assert_eq!(mode1.datagrams, 0, "mode 1 emitted a datagram");
    assert!(!mode1.refused);
    assert_eq!(mode1.record.as_deref(), Some(record.as_slice()));

    let refused = exchange_picture(&cert, &key, QUIC_DATAGRAMS, false, &record)
        .await
        .expect("mode 2 refused");
    assert!(refused.refused);
    assert_eq!(refused.datagrams, 0);
    assert!(refused.record.is_none());

    let mode2 = exchange_picture(&cert, &key, QUIC_DATAGRAMS, true, &record)
        .await
        .expect("mode 2");
    assert!(!mode2.refused);
    assert!(mode2.datagrams > 1, "datagrams {}", mode2.datagrams);
    assert!(
        mode2.stream_bytes.windows(9).any(|window| window == b"carrier 2"),
        "config did not finish on the stream"
    );
    assert_eq!(mode2.record.as_deref(), Some(record.as_slice()));
}

/// Desktop host path: `host::run` after a real `carrier 2` offer. Config stays
/// on the uni stream. Picture and audio are datagrams, assembled by the
/// player's lane demux, and a record bigger than one datagram arrives whole.
#[tokio::test]
async fn host_picture_and_audio_ride_datagrams_after_carrier_2_config() {
    use std::net::{Ipv4Addr, SocketAddr, UdpSocket};
    use std::sync::Arc;
    use std::time::Duration;

    use etherquic_media::host::{self, FrameSlot, HostOptions};
    use etherquic_media::negotiate::{
        Audio, HostCaps, OfferInput, Video, VideoCodec, RECORD_DATA,
    };
    use etherquic_media::{wire, Pcm};

    etherquic::install_pure_crypto();
    let (cert, key) = certs();
    let probe = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).unwrap();
    let port = probe.local_addr().unwrap().port();
    drop(probe);
    let listeners = etherquic::udp_listeners(port, &cert, &key);
    let server = listeners.v4.clone().expect("ipv4 listener");
    let port = listeners.port;

    let offer = etherquic_media::negotiate::build_offer(&OfferInput {
        video_order: &[Video::Mjpeg],
        audio_order: &[Audio::Pcm],
        decodes: &[VideoCodec::Mjpeg],
        quality: 90,
        carrier: Some(QUIC_DATAGRAMS),
        ..Default::default()
    });

    let (cursor_tx, cursor_rx) = tokio::sync::watch::channel(None);
    let host_task = tokio::spawn(async move {
        let conn = server.accept().await.unwrap().await.unwrap();
        let (negotiated, _) = host::accept_offer(&conn).await.unwrap();
        let streams = host::open_streams(&conn, negotiated.is_v2())
            .await
            .unwrap();
        let slot = FrameSlot::new();
        let feeder_slot = slot.clone();
        let feeder = std::thread::spawn(move || {
            for i in 0..100u32 {
                feeder_slot.put(noisy_frame(i));
                std::thread::sleep(Duration::from_millis(40));
            }
            feeder_slot.close();
        });
        let (pcm_tx, pcm_rx) = tokio::sync::mpsc::channel(4);
        tokio::spawn(async move {
            for _ in 0..6 {
                let pcm = Pcm {
                    sample_rate: 48_000,
                    channels: 2,
                    samples: vec![400i16; 960 * 2],
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
            caps: HostCaps::baseline(),
            log: Arc::new(|line: &str| eprintln!("host: {line}")),
            cap_kbps: std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0)),
            carrier_tap: None,
        };
        let result = host::run(streams, negotiated, slot, pcm_rx, options, cursor_rx).await;
        let _ = feeder.join();
        result
    });

    let endpoint = etherquic::udp_client_endpoint_on("127.0.0.1:0".parse().unwrap()).unwrap();
    let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    let conn = endpoint.connect(addr, "localhost").unwrap().await.unwrap();
    let mut offer_send = etherquic_media::player::send_offer(&conn, &offer)
        .await
        .unwrap();
    let _ = &mut offer_send;

    let mut video_cfg = None;
    let mut audio_cfg = None;
    let mut video_recv = None;
    let mut audio_recv = None;
    for _ in 0..2 {
        let mut recv = conn.accept_uni().await.unwrap();
        let magic = wire::read_magic(&mut recv).await.unwrap().unwrap();
        let record = wire::read_record(&mut recv, &magic).await.unwrap().unwrap();
        if &magic == b"EVI2" {
            video_cfg = Some(record);
            video_recv = Some(recv);
        } else if &magic == b"EAU2" {
            audio_cfg = Some(record);
            audio_recv = Some(recv);
        } else {
            panic!("unexpected stream {}", String::from_utf8_lossy(&magic));
        }
    }
    let video_cfg = video_cfg.expect("video config");
    let audio_cfg = audio_cfg.expect("audio config");
    assert!(
        video_cfg.windows(9).any(|window| window == b"carrier 2"),
        "video config was not on the stream"
    );
    assert!(
        audio_cfg.windows(9).any(|window| window == b"carrier 2"),
        "audio config was not on the stream"
    );

    let (picture_tx, mut picture_rx) = tokio::sync::mpsc::channel(8);
    let (audio_tx, mut audio_rx) = tokio::sync::mpsc::channel(8);
    etherquic_media::player::spawn_lane_demux(conn.clone(), picture_tx, audio_tx);
    let picture = tokio::time::timeout(Duration::from_secs(8), picture_rx.recv())
        .await
        .expect("picture datagram record")
        .expect("picture channel");
    let audio = tokio::time::timeout(Duration::from_secs(8), audio_rx.recv())
        .await
        .expect("audio datagram record")
        .expect("audio channel");
    assert_eq!(picture.first().copied(), Some(RECORD_DATA));
    assert_eq!(audio.first().copied(), Some(RECORD_DATA));
    let cursor = etherquic::cursor::CursorUpdate {
        visible: true,
        shape: etherquic::cursor::CursorShape::Arrow,
        x: 12,
        y: 34,
        hotspot_x: 0,
        hotspot_y: 0,
        image_id: 7,
        width: 0,
        height: 0,
        bgra: Vec::new(),
    };
    cursor_tx.send(Some(cursor.encode())).expect("cursor watch");
    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    let mut cursor_record = None;
    while tokio::time::Instant::now() < deadline {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        let Some(record) = tokio::time::timeout(remaining, picture_rx.recv())
            .await
            .ok()
            .flatten()
        else {
            break;
        };
        if record.first().copied() == Some(etherquic_media::negotiate::RECORD_CURSOR) {
            cursor_record = Some(record);
            break;
        }
    }
    let cursor_record = cursor_record.expect("cursor did not arrive as a picture datagram");
    assert!(
        !picture.windows(cursor_record.len()).any(|window| window == cursor_record.as_slice()),
        "cursor bytes were the picture record"
    );
    assert!(
        picture.len() > 1452,
        "picture fit in one datagram: {} bytes",
        picture.len()
    );
    assert!(
        audio.len() > 1452,
        "audio fit in one datagram: {} bytes",
        audio.len()
    );
    assert!(
        !video_cfg.windows(picture.len()).any(|window| window == picture.as_slice()),
        "picture record was on the video stream"
    );
    assert!(
        !audio_cfg.windows(audio.len()).any(|window| window == audio.as_slice()),
        "audio record was on the audio stream"
    );

    let mut video_recv = video_recv.unwrap();
    let mut audio_recv = audio_recv.unwrap();
    let extra_video = tokio::time::timeout(
        Duration::from_millis(300),
        wire::read_record(&mut video_recv, b"EVI2"),
    )
    .await;
    let extra_audio = tokio::time::timeout(
        Duration::from_millis(300),
        wire::read_record(&mut audio_recv, b"EAU2"),
    )
    .await;
    // The host may finish the stream. A finished stream is `Ok(None)`. A
    // picture or audio payload would be `Ok(Some(_))`.
    assert!(
        !matches!(extra_video, Ok(Ok(Some(_)))),
        "video stream carried another record after config: {extra_video:?}"
    );
    assert!(
        !matches!(extra_audio, Ok(Ok(Some(_)))),
        "audio stream carried another record after config: {extra_audio:?}"
    );

    conn.close(0u32.into(), b"done");
    let _ = host_task.await;
}

/// A partial picture and a partial audio record are in the demux. The same
/// pulse a later config record sends drops both, and the next record is the
/// one that arrives.
#[tokio::test]
async fn a_later_config_drops_a_partial_datagram() {
    use std::net::{Ipv4Addr, SocketAddr, UdpSocket};
    use std::time::Duration;

    etherquic::install_pure_crypto();
    let (cert, key) = certs();
    let probe = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).unwrap();
    let port = probe.local_addr().unwrap().port();
    drop(probe);
    let listeners = etherquic::udp_listeners(port, &cert, &key);
    let server = listeners.v4.clone().expect("ipv4 listener");
    let port = listeners.port;
    let endpoint = etherquic::udp_client_endpoint_on("127.0.0.1:0".parse().unwrap()).unwrap();
    let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    let dial = endpoint.connect(addr, "localhost").unwrap();
    let accepted = async { server.accept().await.unwrap().await.unwrap() };
    let (player, host) = tokio::join!(dial, accepted);
    let player = player.expect("player dial");
    let (picture_tx, mut picture_rx) = tokio::sync::mpsc::channel(8);
    let (audio_tx, mut audio_rx) = tokio::sync::mpsc::channel(8);
    let reset = etherquic_media::player::spawn_lane_demux(player, picture_tx, audio_tx);

    let picture_lane = etherquic::carrier::Lane::Picture as u8;
    let audio_lane = etherquic::carrier::Lane::Audio as u8;
    let partial_picture = etherquic::carrier::split_record(1, b"partial-picture", 4);
    let partial_audio = etherquic::carrier::split_record(3, b"partial-audio!!", 4);
    let mut first = vec![picture_lane];
    first.extend(partial_picture[0].encode());
    host.send_datagram(first.into()).unwrap();
    let mut first_audio = vec![audio_lane];
    first_audio.extend(partial_audio[0].encode());
    host.send_datagram(first_audio.into()).unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;
    reset.pulse();

    let mut rest = vec![picture_lane];
    rest.extend(partial_picture[1].encode());
    host.send_datagram(rest.into()).unwrap();
    let fresh = etherquic::carrier::split_record(2, b"new", 8);
    let mut third = vec![picture_lane];
    third.extend(fresh[0].encode());
    host.send_datagram(third.into()).unwrap();
    let mut rest_audio = vec![audio_lane];
    rest_audio.extend(partial_audio[1].encode());
    host.send_datagram(rest_audio.into()).unwrap();
    let tone = etherquic::carrier::split_record(4, b"tone", 8);
    let mut tone_packet = vec![audio_lane];
    tone_packet.extend(tone[0].encode());
    host.send_datagram(tone_packet.into()).unwrap();

    let got = tokio::time::timeout(Duration::from_secs(2), picture_rx.recv())
        .await
        .expect("assembled picture")
        .expect("picture channel");
    assert_eq!(got, b"new");
    let heard = tokio::time::timeout(Duration::from_secs(2), audio_rx.recv())
        .await
        .expect("assembled audio")
        .expect("audio channel");
    assert_eq!(heard, b"tone");
    let late = tokio::time::timeout(Duration::from_millis(150), picture_rx.recv()).await;
    if let Ok(Some(record)) = late {
        assert_ne!(record, b"partial-picture");
    }
}

fn noisy_frame(i: u32) -> etherquic_media::Frame {
    let (width, height) = (320u32, 180u32);
    let mut rgb = vec![0u8; (width * height * 3) as usize];
    let mut n = 0x1234_5678u32.wrapping_add(i.wrapping_mul(997));
    for px in rgb.chunks_exact_mut(3) {
        n = n.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        px[0] = (n >> 16) as u8;
        px[1] = (n >> 8) as u8;
        px[2] = n as u8;
    }
    etherquic_media::Frame {
        width,
        height,
        rgb,
    }
}
