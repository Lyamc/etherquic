//! Live host-to-player matrix. The cell list is [`etherquic::carrier::matrix_sessions`].
//!
//! On AN515 (`COMPUTERNAME` contains `AN515`, or `ETHERQUIC_MATRIX=1`) each cell
//! opens a real session and passes only after a picture of that codec is decoded.
//! `ETHERQUIC_MATRIX=0` checks the cell list and does not dial.
//! `ETHERQUIC_MATRIX_LOG` receives one line per cell.
//! `ETHERQUIC_MATRIX_NIC` is the L2 adapter (default `Ethernet 3` on Windows).

use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr, SocketAddrV6};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use etherquic::carrier::{
    choose_profile, matrix_label, matrix_sessions, set_l3_port, spawn_player_avb, spawn_player_rtp,
    spawn_player_st2110, MatrixLayer, MatrixProfile, MatrixSession, AVB, QUIC_DATAGRAMS, ST2110,
    UDP_RTP,
};
use etherquic_media::host::{self, FrameSlot, HostOptions};
use etherquic_media::negotiate::{
    Audio, FpsWish, HostCaps, OfferInput, Video, VideoCodec, AUDIO2_MAGIC, VIDEO2_MAGIC,
};
use etherquic_media::player::{self, PlayerEvent};
use etherquic_media::{wire, Frame, Pcm};

const HOST_MAC: [u8; 6] = [0x02, 0x00, 0x00, 0x00, 0x00, 0x01];
const CLIENT_MAC: [u8; 6] = [0x02, 0x00, 0x00, 0x00, 0x00, 0x02];
const L2_SERVICE: u16 = 27_133;

fn run_live() -> bool {
    match std::env::var("ETHERQUIC_MATRIX").ok().as_deref() {
        Some("0") => false,
        Some("1") => true,
        _ => std::env::var("COMPUTERNAME")
            .map(|name| name.to_ascii_uppercase().contains("AN515"))
            .unwrap_or(false),
    }
}

fn prepare_native_libs() {
    let dir = PathBuf::from(r"C:\Program Files\etherquic");
    if !dir.is_dir() {
        return;
    }
    let mut path = dir.as_os_str().to_os_string();
    path.push(";");
    if let Some(old) = std::env::var_os("PATH") {
        path.push(old);
    }
    std::env::set_var("PATH", path);
    let ffmpeg = dir.join("ffmpeg.exe");
    if ffmpeg.is_file() && std::env::var_os("ETHERQUIC_FFMPEG").is_none() {
        std::env::set_var("ETHERQUIC_FFMPEG", ffmpeg);
    }
    let dll = dir.join("pyrowave_d3d11.dll");
    if dll.is_file() && std::env::var_os("ETHERQUIC_PYROWAVE_D3D11").is_none() {
        std::env::set_var("ETHERQUIC_PYROWAVE_D3D11", dll);
    }
}

struct Log {
    file: Option<std::fs::File>,
}

impl Log {
    fn open() -> Self {
        let file = std::env::var_os("ETHERQUIC_MATRIX_LOG").and_then(|path| {
            std::fs::File::create(path).ok()
        });
        Self { file }
    }

    fn line(&mut self, text: &str) {
        println!("{text}");
        if let Some(file) = self.file.as_mut() {
            use std::io::Write;
            let _ = writeln!(file, "{text}");
            let _ = file.flush();
        }
    }
}

fn certs() -> (PathBuf, PathBuf) {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../certs");
    (root.join("cert.pem"), root.join("key.pem"))
}

fn frame(i: u32) -> Frame {
    let (w, h) = (64u32, 64u32);
    let mut rgb = vec![0u8; (w * h * 3) as usize];
    for (p, px) in rgb.chunks_exact_mut(3).enumerate() {
        px[0] = (p as u32).wrapping_add(i.wrapping_mul(17)) as u8;
        px[1] = (p as u32 / 8).wrapping_add(i) as u8;
        px[2] = 40u8.wrapping_add(i as u8);
    }
    Frame {
        width: w,
        height: h,
        rgb,
    }
}

fn family(video: Video) -> &'static str {
    match video.codec() {
        VideoCodec::Mjpeg => "mjpeg",
        VideoCodec::Pyrowave => "pyrowave",
        VideoCodec::H264 => "h264",
        _ => "other",
    }
}

fn rows_for(codec: &str, caps: &HostCaps) -> Vec<Video> {
    match codec {
        "mjpeg" => vec![Video::Mjpeg],
        "pyrowave" => caps
            .video
            .iter()
            .map(|row| row.video)
            .filter(|video| matches!(video, Video::Pyrowave | Video::PyrowaveCpu))
            .collect(),
        "h264" => caps
            .video
            .iter()
            .map(|row| row.video)
            .filter(|video| matches!(video, Video::H264Hw | Video::H264Sw))
            .collect(),
        _ => Vec::new(),
    }
}

fn codec_id(codec: &str) -> VideoCodec {
    match codec {
        "pyrowave" => VideoCodec::Pyrowave,
        "h264" => VideoCodec::H264,
        _ => VideoCodec::Mjpeg,
    }
}

struct V6 {
    ip: Ipv6Addr,
    scope: u32,
}

struct Dial {
    v4: Ipv4Addr,
    v6: Option<V6>,
}

fn nic_name() -> String {
    std::env::var("ETHERQUIC_MATRIX_NIC").unwrap_or_else(|_| {
        if cfg!(windows) {
            "Ethernet 3".into()
        } else {
            String::new()
        }
    })
}

fn dial_from_pcap(nic: &str) -> Dial {
    let want = if nic.is_empty() { None } else { Some(nic) };
    let info = etherquic::pcap_link::interface_info(want).ok();
    let v4 = info
        .as_ref()
        .and_then(|info| {
            info.ipv4
                .iter()
                .copied()
                .find(|ip| !ip.is_link_local() && !ip.is_loopback())
        })
        .unwrap_or(Ipv4Addr::LOCALHOST);
    let v6 = info.as_ref().and_then(|info| {
        info.ipv6
            .iter()
            .copied()
            .find(|ip| !ip.is_unicast_link_local() && !ip.is_loopback())
            .map(|ip| V6 { ip, scope: 0 })
    });
    Dial { v4, v6 }
}

/// Windows reports the interface index, which is the link-local scope id.
fn dial_from_windows(nic: &str) -> Option<Dial> {
    if !cfg!(windows) || nic.is_empty() {
        return None;
    }
    let script = format!(
        "Get-NetIPAddress -InterfaceAlias '{nic}' -ErrorAction SilentlyContinue | ForEach-Object {{ $_.AddressFamily.ToString() + '|' + $_.IPAddress + '|' + $_.InterfaceIndex }}"
    );
    let output = std::process::Command::new("powershell")
        .args(["-NoProfile", "-Command", &script])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let mut v4 = None;
    let mut v6_global = None;
    let mut v6_link = None;
    for line in text.lines() {
        let mut parts = line.trim().split('|');
        let family = parts.next().unwrap_or("");
        let addr = parts.next().unwrap_or("");
        let index: u32 = parts.next().unwrap_or("0").parse().unwrap_or(0);
        if family.contains("IPv4") || family == "2" || family.eq_ignore_ascii_case("InterNetwork") {
            if let Ok(ip) = addr.parse::<Ipv4Addr>() {
                if !ip.is_loopback() && !ip.is_link_local() && !ip.is_unspecified() {
                    v4 = Some(ip);
                }
            }
        }
        if family.contains("IPv6") || family == "23" || family.eq_ignore_ascii_case("InterNetworkV6") {
            let bare = addr.split('%').next().unwrap_or(addr);
            if let Ok(ip) = bare.parse::<Ipv6Addr>() {
                if ip.is_loopback() || ip.is_unspecified() {
                    continue;
                }
                if ip.is_unicast_link_local() {
                    v6_link = Some(V6 { ip, scope: index });
                } else {
                    v6_global = Some(V6 { ip, scope: 0 });
                }
            }
        }
    }
    Some(Dial {
        v4: v4.unwrap_or(Ipv4Addr::LOCALHOST),
        v6: v6_global.or(v6_link),
    })
}

fn dial_targets(nic: &str) -> Dial {
    dial_from_windows(nic).unwrap_or_else(|| dial_from_pcap(nic))
}

fn v6_socket(v6: &V6, port: u16) -> SocketAddr {
    let mut addr = SocketAddrV6::new(v6.ip, port, 0, 0);
    if v6.ip.is_unicast_link_local() {
        addr.set_scope_id(v6.scope);
    }
    SocketAddr::V6(addr)
}

fn rtp_base(index: usize) -> u16 {
    let start = 26_000u16.saturating_add((index as u16).saturating_mul(8));
    for port in start..start.saturating_add(80) {
        let v4_ok = [0, 1, 2, 4].into_iter().all(|off| {
            std::net::UdpSocket::bind((Ipv4Addr::UNSPECIFIED, port.saturating_add(off))).is_ok()
        });
        if v4_ok {
            return port;
        }
    }
    start
}

fn apply_profile(session: &MatrixSession) {
    match (session.cell.carrier, session.cell.profile) {
        (UDP_RTP, MatrixProfile::Plain) => choose_profile(true, false),
        (UDP_RTP, MatrixProfile::Gcm) => choose_profile(false, false),
        (ST2110, MatrixProfile::Plain) => choose_profile(false, false),
        (ST2110, MatrixProfile::Gcm) => choose_profile(false, true),
        _ => {}
    }
}

struct Endpoints {
    quic_port: u16,
    v4_server: quinn::Endpoint,
    v6_server: Option<quinn::Endpoint>,
    v4_client: quinn::Endpoint,
    v6_client: Option<quinn::Endpoint>,
    l2_server: Option<quinn::Endpoint>,
    l2_client: Option<quinn::Endpoint>,
    l2_net: Option<std::sync::Arc<etherquic::L2Net>>,
    l2_error: Option<String>,
    dial: Dial,
}

fn free_quic_port() -> u16 {
    let probe = std::net::UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).expect("probe");
    probe.local_addr().expect("probe addr").port()
}

fn open_endpoints(cert: &std::path::Path, key: &std::path::Path, nic: &str) -> Result<Endpoints, String> {
    let listeners = etherquic::udp_listeners(free_quic_port(), cert, key);
    let v4_server = listeners.v4.clone().ok_or_else(|| listeners.note.clone())?;
    let v6_server = listeners.v6.clone();
    let v4_client = etherquic::udp_client_endpoint_on("0.0.0.0:0".parse().unwrap())
        .map_err(|error| error.to_string())?;
    let v6_client = etherquic::udp_client_endpoint_on("[::]:0".parse().unwrap()).ok();
    let dial = dial_targets(nic);
    let (l2_net, l2_server, l2_client, l2_error) = match etherquic::L2Net::open(
        (!nic.is_empty()).then_some(nic),
        1500,
        etherquic::ETHERTYPE_QUIC,
    ) {
        Ok(net) => {
            match (
                etherquic::l2_server_endpoint(net.clone(), HOST_MAC, L2_SERVICE, cert, key),
                etherquic::l2_client_endpoint(net.clone(), CLIENT_MAC, L2_SERVICE),
            ) {
                (Ok(server), Ok(client)) => (Some(net), Some(server), Some(client), None),
                (Err(error), _) | (_, Err(error)) => (
                    Some(net),
                    None,
                    None,
                    Some(error.to_string()),
                ),
            }
        }
        Err(error) => (None, None, None, Some(error.to_string())),
    };
    Ok(Endpoints {
        quic_port: listeners.port,
        v4_server,
        v6_server,
        v4_client,
        v6_client,
        l2_server,
        l2_client,
        l2_net,
        l2_error,
        dial,
    })
}

struct CellPass {
    video: Video,
    frames: u64,
}

async fn run_cell(
    session: &MatrixSession,
    index: usize,
    caps: &HostCaps,
    live: &Endpoints,
) -> Result<CellPass, String> {
    let rows = rows_for(session.codec, caps);
    if rows.is_empty() {
        let have: Vec<&str> = caps.video.iter().map(|row| row.video.token()).collect();
        return Err(format!(
            "host caps have no {} encoder ({})",
            session.codec,
            have.join(" ")
        ));
    }
    apply_profile(session);
    let media = session.cell.carrier >= UDP_RTP;
    let hello = if media {
        let port = rtp_base(index);
        set_l3_port(port);
        let input = port.saturating_add(2);
        match session.cell.layer {
            MatrixLayer::Ipv6 => {
                let v6 = live.dial.v6.as_ref().ok_or("no IPv6 address on the test NIC")?;
                Some(v6_socket(v6, input))
            }
            _ => Some(SocketAddr::from((live.dial.v4, input))),
        }
    } else {
        set_l3_port(0);
        None
    };
    if session.cell.layer == MatrixLayer::L2 && live.l2_server.is_none() {
        return Err(live.l2_error.clone().unwrap_or_else(|| "L2 did not open".into()));
    }
    if session.cell.layer == MatrixLayer::Ipv6 && live.v6_server.is_none() {
        return Err("IPv6 QUIC listener did not bind".into());
    }

    let (pic_tx, pic_rx) = tokio::sync::mpsc::channel(8);
    let (aud_tx, aud_rx) = tokio::sync::mpsc::channel(8);
    let mut pic_rx = Some(pic_rx);
    let mut aud_rx = Some(aud_rx);
    if let Some(hello) = hello {
        match session.cell.carrier {
            UDP_RTP => spawn_player_rtp(hello, pic_tx, aud_tx),
            AVB => spawn_player_avb(hello, pic_tx, aud_tx),
            ST2110 => spawn_player_st2110(hello, pic_tx, aud_tx),
            _ => {
                drop(pic_tx);
                drop(aud_tx);
                pic_rx = None;
                aud_rx = None;
            }
        }
        tokio::time::sleep(Duration::from_millis(80)).await;
    } else {
        drop(pic_tx);
        drop(aud_tx);
        pic_rx = None;
        aud_rx = None;
    }

    let server = match session.cell.layer {
        MatrixLayer::L2 => live.l2_server.clone().ok_or("L2 server missing")?,
        MatrixLayer::Ipv4 => live.v4_server.clone(),
        MatrixLayer::Ipv6 => live.v6_server.clone().ok_or("IPv6 server missing")?,
    };
    let host_log: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let log = host_log.clone();
    let caps = caps.clone();
    let host_task = tokio::spawn(async move {
        let incoming = server
            .accept()
            .await
            .ok_or_else(|| "accept ended".to_string())?;
        let conn = incoming.await.map_err(|error| error.to_string())?;
        let (negotiated, _) = host::accept_offer(&conn).await?;
        let streams = host::open_streams(&conn, negotiated.is_v2()).await?;
        let slot = FrameSlot::new();
        let feeder_slot = slot.clone();
        let stop = Arc::new(AtomicBool::new(false));
        let stop_feed = stop.clone();
        let feeder = std::thread::spawn(move || {
            let mut i = 0u32;
            while !stop_feed.load(Ordering::Relaxed) && i < 240 {
                feeder_slot.put(frame(i));
                i += 1;
                std::thread::sleep(Duration::from_millis(40));
            }
            feeder_slot.close();
        });
        let (pcm_tx, pcm_rx) = tokio::sync::mpsc::channel(4);
        let stop_audio = stop.clone();
        tokio::spawn(async move {
            while !stop_audio.load(Ordering::Relaxed) {
                let pcm = Pcm {
                    sample_rate: 48_000,
                    channels: 2,
                    samples: vec![1i16; 960 * 2],
                };
                if pcm_tx.send(pcm).await.is_err() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        });
        let options = HostOptions {
            fps: FpsWish::Fixed(15),
            display_hz: None,
            caps,
            log: Arc::new(move |line: &str| {
                let mut lines = log.lock().unwrap_or_else(|poison| poison.into_inner());
                if lines.len() > 40 {
                    lines.remove(0);
                }
                lines.push(line.to_string());
            }),
            cap_kbps: Arc::new(AtomicU64::new(0)),
            carrier_tap: None,
        };
        let (_cursor_tx, cursor_rx) = tokio::sync::watch::channel(None);
        let result = host::run(streams, negotiated, slot, pcm_rx, options, cursor_rx).await;
        stop.store(true, Ordering::Relaxed);
        let _ = feeder.join();
        result
    });

    let dial = match session.cell.layer {
        MatrixLayer::L2 => etherquic::peer_to_socket_addr(HOST_MAC, L2_SERVICE),
        MatrixLayer::Ipv4 => SocketAddr::from((live.dial.v4, live.quic_port)),
        MatrixLayer::Ipv6 => {
            let v6 = live.dial.v6.as_ref().ok_or("no IPv6 address on the test NIC")?;
            v6_socket(v6, live.quic_port)
        }
    };
    let connecting = match session.cell.layer {
        MatrixLayer::L2 => live
            .l2_client
            .as_ref()
            .ok_or("L2 client missing")?
            .connect(dial, "localhost")
            .map_err(|error| error.to_string())?,
        MatrixLayer::Ipv4 => live
            .v4_client
            .connect(dial, "localhost")
            .map_err(|error| error.to_string())?,
        MatrixLayer::Ipv6 => live
            .v6_client
            .as_ref()
            .ok_or("IPv6 client missing")?
            .connect(dial, "localhost")
            .map_err(|error| error.to_string())?,
    };
    let conn = tokio::time::timeout(Duration::from_secs(8), connecting)
        .await
        .map_err(|_| "connect timed out".to_string())?
        .map_err(|error| error.to_string())?;

    let decode = codec_id(session.codec);
    let offer = etherquic_media::negotiate::build_offer(&OfferInput {
        video_order: &rows,
        audio_order: &[Audio::Pcm],
        decodes: &[decode],
        quality: 70,
        carrier: Some(session.cell.carrier),
        fps: FpsWish::Fixed(15),
        want_dirty_rects: false,
        ..OfferInput::default()
    });
    let _offer_send = player::send_offer(&conn, &offer).await?;

    let mut reset = None;
    if session.cell.carrier == QUIC_DATAGRAMS {
        let (tx, rx) = tokio::sync::mpsc::channel(8);
        let (atx, arx) = tokio::sync::mpsc::channel(8);
        reset = Some(player::spawn_lane_demux(conn.clone(), tx, atx));
        pic_rx = Some(rx);
        aud_rx = Some(arx);
    }

    let frames = Arc::new(AtomicU64::new(0));
    let counted = frames.clone();
    let sink: etherquic_media::Sink<Frame> = Arc::new(move |frame| {
        let bytes = (frame.width as usize).saturating_mul(frame.height as usize).saturating_mul(3);
        if frame.width > 0 && frame.height > 0 && frame.rgb.len() == bytes {
            counted.fetch_add(1, Ordering::Relaxed);
        }
        true
    });
    let negotiated: Arc<Mutex<Option<Video>>> = Arc::new(Mutex::new(None));
    let failed: Arc<Mutex<String>> = Arc::new(Mutex::new(String::new()));
    let (events_tx, mut events_rx) = tokio::sync::mpsc::unbounded_channel();
    let got = negotiated.clone();
    let fail = failed.clone();
    let events = tokio::spawn(async move {
        while let Some(event) = events_rx.recv().await {
            match event {
                PlayerEvent::Video(config) => {
                    *got.lock().unwrap_or_else(|poison| poison.into_inner()) = config.video;
                }
                PlayerEvent::VideoFailed { video, reason } => {
                    let mut slot = fail.lock().unwrap_or_else(|poison| poison.into_inner());
                    if slot.is_empty() {
                        *slot = format!("{}: {reason}", video.token());
                    }
                }
                _ => {}
            }
        }
    });

    let (pcm_tx, mut pcm_rx) = tokio::sync::mpsc::channel::<Pcm>(8);
    tokio::spawn(async move { while pcm_rx.recv().await.is_some() {} });
    let mut video_task = None;
    let mut audio_task = None;
    for _ in 0..2 {
        let mut recv = tokio::time::timeout(Duration::from_secs(8), conn.accept_uni())
            .await
            .map_err(|_| "no media stream".to_string())?
            .map_err(|error| error.to_string())?;
        let magic = tokio::time::timeout(Duration::from_secs(8), wire::read_magic(&mut recv))
            .await
            .map_err(|_| "media stream header timed out".to_string())?
            .map_err(|error| error.to_string())?
            .ok_or("empty media stream")?;
        if &magic == VIDEO2_MAGIC {
            video_task = Some(tokio::spawn(player::read_video(
                recv,
                true,
                sink.clone(),
                events_tx.clone(),
                Some(session.cell.carrier),
                pic_rx.take(),
                reset.take(),
            )));
        } else if &magic == AUDIO2_MAGIC {
            audio_task = Some(tokio::spawn(player::read_audio(
                recv,
                true,
                pcm_tx.clone(),
                events_tx.clone(),
                aud_rx.take(),
                None,
            )));
        }
    }
    drop(events_tx);
    drop(pcm_tx);

    let limit = if session.codec == "mjpeg" { 12 } else { 25 };
    let deadline = tokio::time::Instant::now() + Duration::from_secs(limit);
    while frames.load(Ordering::Relaxed) == 0 && tokio::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    conn.close(0u32.into(), b"done");
    // The next cell opens the same D3D11 and NDIS devices. Wait until this
    // cell's tasks have actually stopped, or the following cell faults.
    host_task.abort();
    let _ = tokio::time::timeout(Duration::from_secs(3), host_task).await;
    if let Some(task) = video_task {
        task.abort();
        let _ = tokio::time::timeout(Duration::from_secs(3), task).await;
    }
    if let Some(task) = audio_task {
        task.abort();
        let _ = tokio::time::timeout(Duration::from_secs(3), task).await;
    }
    events.abort();

    let decoded = frames.load(Ordering::Relaxed);
    let video = negotiated
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .clone();
    if decoded == 0 {
        let host = host_log
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .join(" | ");
        let why = failed
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .clone();
        let mut msg = "no decoded picture".to_string();
        if !why.is_empty() {
            msg.push_str(&format!(" ({why})"));
        }
        if !host.is_empty() {
            msg.push_str(&format!(" host: {host}"));
        }
        return Err(msg);
    }
    let Some(video) = video else {
        return Err(format!("decoded {decoded} frames with no video config"));
    };
    if family(video) != session.codec {
        return Err(format!(
            "decoded {decoded} frames as {}, asked for {}",
            video.token(),
            session.codec
        ));
    }
    Ok(CellPass {
        video,
        frames: decoded,
    })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an515_decodes_every_protocol_layer_profile_and_codec() {
    let sessions = matrix_sessions();
    assert_eq!(sessions.len(), 57, "the matrix enumerator drifted");
    let mut log = Log::open();
    let live_run = run_live();
    let computer = std::env::var("COMPUTERNAME").unwrap_or_else(|_| "unknown".into());
    log.line(&format!(
        "MATRIX_BEGIN host={computer} live={live_run} cells={}",
        sessions.len()
    ));
    if !live_run {
        log.line("MATRIX_RESULT skipped=1 passed=0 failed=0");
        return;
    }

    prepare_native_libs();
    etherquic::install_pure_crypto();
    let (cert, key) = certs();
    let nic = nic_name();
    let endpoints = match open_endpoints(&cert, &key, &nic) {
        Ok(endpoints) => endpoints,
        Err(error) => {
            log.line(&format!("MATRIX_RESULT skipped=0 passed=0 failed=57 open: {error}"));
            panic!("matrix endpoints: {error}");
        }
    };
    let v6 = endpoints
        .dial
        .v6
        .as_ref()
        .map(|v6| format!("{} scope {}", v6.ip, v6.scope))
        .unwrap_or_else(|| "none".into());
    let adapter = endpoints
        .l2_net
        .as_ref()
        .and_then(|net| net.adapter())
        .unwrap_or("");
    log.line(&format!(
        "MATRIX_DIAL nic={nic} adapter={adapter} ipv4={} ipv6={v6} quic={} l2={}",
        endpoints.dial.v4,
        endpoints.quic_port,
        endpoints
            .l2_error
            .clone()
            .unwrap_or_else(|| "open".into())
    ));
    let caps = etherquic_media::caps::host_caps();
    let have: Vec<String> = caps
        .video
        .iter()
        .map(|row| format!("{}={}", row.video.token(), row.encoder))
        .collect();
    log.line(&format!("MATRIX_CAPS {}", have.join(" ")));

    let mut passed = 0u32;
    let mut failed = Vec::new();
    for (index, session) in sessions.iter().enumerate() {
        set_l3_port(0);
        let label = matrix_label(session);
        log.line(&format!("MATRIX_CELL START {label}"));
        let started = std::time::Instant::now();
        match run_cell(session, index, &caps, &endpoints).await {
            Ok(cell) => {
                passed += 1;
                log.line(&format!(
                    "MATRIX_CELL PASS {label} decoded={} frames={} ms={}",
                    cell.video.token(),
                    cell.frames,
                    started.elapsed().as_millis()
                ));
            }
            Err(error) => {
                log.line(&format!(
                    "MATRIX_CELL FAIL {label} ms={} {error}",
                    started.elapsed().as_millis()
                ));
                failed.push(format!("{label}: {error}"));
            }
        }
    }
    set_l3_port(0);
    log.line(&format!(
        "MATRIX_RESULT skipped=0 passed={passed} failed={}",
        failed.len()
    ));
    assert!(
        failed.is_empty(),
        "{} cells failed:\n{}",
        failed.len(),
        failed.join("\n")
    );
}
