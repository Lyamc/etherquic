//! Capture the primary monitor and its audio, and inject player input.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU32, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{Sample, SampleFormat};
use enigo::{Axis, Button, Coordinate, Direction, Enigo, Key, Keyboard, Mouse, Settings};
use etherquic::{install_pure_crypto, l2_server_endpoint, udp_accept, udp_listeners, L2Net};
use etherquic_input::{Body, Buttons, Event, KeySource};
use tokio::sync::{broadcast, mpsc};
use xcap::Monitor;

use crate::desk_io::{self, NoteSide};
use crate::pad_inject::Pads;
use crate::{
    desktop_audio, fit_rgba, format_mac, pad_report, place_pointer, serve_session, AudioChunk,
    DesktopAudio, PointerPlace, Status,
};

/// Refresh rate of the monitor `host` captures, in whole frames per second.
///
/// `None` when the display rate cannot be read. The primary monitor is the
/// one the capture loop uses.
pub fn display_hz() -> Option<u32> {
    let monitors = Monitor::all().ok()?;
    let index = monitors
        .iter()
        .position(|monitor| monitor.is_primary().unwrap_or(false))
        .unwrap_or(0);
    let monitor = monitors.get(index)?;
    let hz = monitor.frequency().ok()?;
    if !hz.is_finite() || hz < 1.0 {
        return None;
    }
    let rounded = hz.round() as u32;
    (1..=240).contains(&rounded).then_some(rounded)
}

/// What `host` shares. The NIC is already open.
pub struct HostSession {
    pub local_mac: [u8; 6],
    pub service_id: u16,
    pub max_width: u32,
    pub fps: etherquic::negotiate::FpsWish,
    pub cert: PathBuf,
    pub key: PathBuf,
    /// Desktop audio source by device name. `None` picks one as before.
    pub audio_device: Option<String>,
}

/// Share this desktop until `stop` is set.
///
/// `up` is set once the listener exists. `peers` is the number of connected viewers.
pub fn host_desktop(
    net: Arc<L2Net>,
    host: HostSession,
    stop: Arc<AtomicBool>,
    up: Arc<AtomicBool>,
    peers: Arc<AtomicUsize>,
    status: Status,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // L2/pcap is already open. Drop CAP_NET_* so Plasma's portal can
    // Screenshot this process (otherwise /proc/self/root is blocked).
    etherquic::release_net_capabilities_for_portals();
    let rt = tokio::runtime::Runtime::new()?;
    let result = rt.block_on(listen(net, host, stop, up, peers, status));
    rt.shutdown_timeout(Duration::from_secs(2));
    result
}

async fn listen(
    net: Arc<L2Net>,
    host: HostSession,
    stop: Arc<AtomicBool>,
    up: Arc<AtomicBool>,
    peers: Arc<AtomicUsize>,
    status: Status,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    install_pure_crypto();
    let screen = Arc::new(Screen {
        width: AtomicU32::new(1),
        height: AtomicU32::new(1),
        frame_w: AtomicU32::new(0),
        frame_h: AtomicU32::new(0),
        origin_x: AtomicI32::new(0),
        origin_y: AtomicI32::new(0),
    });
    let (cursor_tx, cursor_rx) = tokio::sync::watch::channel(None);
    let frames = etherquic_media::host::FrameSlot::new();
    let video_tx = frames.clone();
    let (audio_tx, _audio_hold) = broadcast::channel(32);
    etherquic_media::caps::warm();
    let (input_tx, mut input_rx) = mpsc::channel::<Vec<Event>>(32);

    let capture_stop = Arc::clone(&stop);
    let capture_screen = screen.clone();
    let max_width = host.max_width.max(2);
    let display_rate = display_hz();
    let encode_wish = host.fps;
    let capture_fps = display_rate.unwrap_or(etherquic::negotiate::DEFAULT_CAPTURE_FPS);
    let mut capture = Some(
        thread::Builder::new()
            .name("etherquic-play-capture".into())
            .spawn(move || {
                capture_loop(
                    capture_stop,
                    capture_screen,
                    video_tx,
                    cursor_tx,
                    max_width,
                    capture_fps,
                )
            })?,
    );
    let audio_stop = Arc::clone(&stop);
    let audio_device = host.audio_device.clone();
    let capture_audio_tx = audio_tx.clone();
    let mut audio = Some(
        thread::Builder::new()
            .name("etherquic-play-audio".into())
            .spawn(move || {
                if let Err(error) =
                    capture_audio(audio_stop, capture_audio_tx, audio_device.as_deref())
                {
                    eprintln!("etherquic-play: audio capture stopped: {error}");
                }
            })?,
    );

    let endpoint =
        match l2_server_endpoint(
            Arc::clone(&net),
            host.local_mac,
            host.service_id,
            &host.cert,
            &host.key,
        ) {
            Ok(endpoint) => endpoint,
            Err(error) => {
                release_host(&stop, &up, &peers, &frames, &mut capture, &mut audio);
                return Err(error.into());
            }
        };
    up.store(true, Ordering::SeqCst);
    let udp = udp_listeners(host.service_id, &host.cert, &host.key);
    etherquic::carrier::enable_on_host(udp.port);
    let udp_note = udp.note.clone();
    let udp_v4 = udp.v4;
    let udp_v6 = udp.v6;
    // Announce the port that actually bound. Another program (RoSE on 4433)
    // makes that the next free port, and players dial the one in the offer.
    let discovery = start_discovery(&net, &host, udp.port, Arc::clone(&stop));
    let mac = format_mac(host.local_mac);
    status(&format!(
        "Sharing this desktop as {mac} on service {}. {udp_note} Waiting for players.",
        host.service_id
    ));

    let inject_screen = screen.clone();
    let inject_stop = Arc::clone(&stop);
    let (ack_tx, ack_rx) = mpsc::channel(64);
    let (ack_bcast, _) = broadcast::channel::<crate::ack::InputAck>(64);
    tokio::spawn({
        let ack_bcast = ack_bcast.clone();
        let mut ack_rx = ack_rx;
        async move {
            while let Some(ack) = ack_rx.recv().await {
                let _ = ack_bcast.send(ack);
            }
        }
    });
    // Injection holds platform seats (CGEventSource, libei) that must not sit
    // on the Tokio runtime. A dedicated thread matches capture and audio.
    let mut inject = Some(
        thread::Builder::new()
            .name("etherquic-play-inject".into())
            .spawn(move || {
                let mut hands = None;
                loop {
                    if inject_stop.load(Ordering::SeqCst) {
                        break;
                    }
                    let events = match input_rx.try_recv() {
                        Ok(events) => events,
                        Err(mpsc::error::TryRecvError::Empty) => {
                            thread::sleep(Duration::from_millis(5));
                            continue;
                        }
                        Err(mpsc::error::TryRecvError::Disconnected) => break,
                    };
                    if let Some(slot) = recorded_input() {
                        slot.lock()
                            .unwrap_or_else(|poison| poison.into_inner())
                            .extend(events);
                        continue;
                    }
                    let hands = hands.get_or_insert_with(Hands::open);
                    let width = inject_screen.width.load(Ordering::Relaxed).max(1);
                    let height = inject_screen.height.load(Ordering::Relaxed).max(1);
                    hands.apply(&events, width, height, &ack_tx);
                }
            })?,
    );

    let codec_status = status.clone();
    let log: etherquic_media::Log = Arc::new(move |line: &str| {
        eprintln!("etherquic-play: {line}");
        if line.starts_with("video ") || line.starts_with("audio ") {
            codec_status(&format!("Sending {line}."));
        }
    });
    let caps = etherquic_media::caps::host_caps();
    let options = etherquic_media::host::HostOptions {
        fps: encode_wish,
        display_hz: display_rate,
        caps,
        log: log.clone(),
        cap_kbps: std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0)),
        carrier_tap: Some(etherquic::carrier::open_carrier_tap()),
    };
    let keyframes = etherquic_media::host::KeyframeBus::default();
    let hub = etherquic_media::host::EncodeHub::new(frames.clone(), keyframes.clone(), &options);
    let room = etherquic_media::host::Room::default();
    let shared = etherquic_media::host::SharedEncode {
        keyframes,
        hub: Some(hub),
        room: Some(room.clone()),
    };
    let (cmd_tx, pipes, outbound, inbound) = desk_io::open_notes(desk_io::desk_roster());
    desk_io::attach_host(cmd_tx);
    tokio::spawn(desk_io::pump_outbound(outbound));
    tokio::spawn(desk_io::pump_inbound(NoteSide::Host, inbound));
    let notes = Arc::new(std::sync::Mutex::new(Some(pipes)));

    let mut sessions: Vec<tokio::task::JoinHandle<()>> = Vec::new();
    let mut carrier_audio = audio_tx.subscribe();
    let mut carrier_served = false;
    loop {
        sessions.retain(|task| !task.is_finished());
        let wake = tokio::select! {
            incoming = endpoint.accept() => HostWake::Conn(incoming),
            incoming = udp_accept(udp_v4.as_ref()) => HostWake::Conn(incoming),
            incoming = udp_accept(udp_v6.as_ref()) => HostWake::Conn(incoming),
            _ = until_stop(&stop) => HostWake::Stop,
            _ = tokio::time::sleep(Duration::from_millis(200)) => HostWake::Tick,
        };
        let incoming = match wake {
            HostWake::Stop => break,
            HostWake::Tick => {
                // A QUIC session already carries this carrier. The one-frame
                // hello path is only for a player that never opens QUIC.
                if sessions.is_empty() {
                    offer_carrier_frame(&frames, &mut carrier_audio, &mut carrier_served);
                }
                continue;
            }
            HostWake::Conn(incoming) => incoming,
        };
        let Some(incoming) = incoming else {
            status("Host endpoint closed.");
            break;
        };
        if room.locked() {
            incoming.refuse();
            status("Exclusive session; a later join was refused.");
            continue;
        }
        // Hold the one-frame path while this handshake is in flight.
        carrier_served = true;
        let conn = tokio::select! {
            result = incoming => match result {
                Ok(conn) => conn,
                Err(error) => {
                    eprintln!("etherquic-play: handshake failed: {error}");
                    if sessions.is_empty() {
                        carrier_served = false;
                    }
                    continue;
                }
            },
            _ = until_stop(&stop) => break,
        };
        let n = peers.fetch_add(1, Ordering::SeqCst) + 1;
        status(&format!(
            "{n} player{} connected. Last join {}.",
            if n == 1 { "" } else { "s" },
            conn.remote_address()
        ));
        let media = crate::HostMedia {
            frames: frames.clone(),
            audio: subscribe_audio(&audio_tx),
            fps: encode_wish,
            display_hz: display_rate,
            log: log.clone(),
            cursor: cursor_rx.clone(),
            shared: shared.clone(),
        };
        let pipes = notes
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .take()
            .unwrap_or_else(crate::Pipes::idle);
        let input = input_tx.clone();
        let acks = subscribe_acks(&ack_bcast);
        let peers = Arc::clone(&peers);
        let status = status.clone();
        sessions.push(tokio::spawn(async move {
            let remote = conn.remote_address();
            let result = serve_session(conn, media, input, Some(acks), pipes).await;
            let left = peers.fetch_sub(1, Ordering::SeqCst).saturating_sub(1);
            if let Err(error) = result {
                eprintln!("etherquic-play: player {remote} left: {error}");
            }
            if left == 0 {
                status("Waiting for players.");
            } else {
                status(&format!(
                    "{left} player{} connected.",
                    if left == 1 { "" } else { "s" }
                ));
            }
        }));
    }
    desk_io::detach_host();
    stop.store(true, Ordering::SeqCst);
    for task in &sessions {
        task.abort();
    }
    drop(input_tx);
    for task in sessions {
        let _ = tokio::time::timeout(Duration::from_secs(2), task).await;
    }
    if let Some(handle) = inject.take() {
        let _ = tokio::task::spawn_blocking(move || handle.join()).await;
    }
    release_host(&stop, &up, &peers, &frames, &mut capture, &mut audio);
    join_discovery(discovery);
    status("Host stopped.");
    Ok(())
}

/// Answers EQDS on UDP 4440 and EtherType 0x88B6 until `stop` is set.
///
/// An in-memory link has no driver, and announcing from it would open the
/// real NIC during tests. A live host always has a backend.
fn start_discovery(
    net: &L2Net,
    host: &HostSession,
    udp_port: u16,
    stop: Arc<AtomicBool>,
) -> Vec<JoinHandle<()>> {
    if net.l2_backend() == 0 {
        return Vec::new();
    }
    let backend = etherquic::selected_backend(&etherquic::probe_backends());
    let (video, audio) = etherquic::offer_media(backend);
    let card = Arc::new(etherquic::discover::HostCard {
        name: computer_name(),
        l2_mac: host.local_mac,
        l2_service: host.service_id,
        l3_port: udp_port,
        video: Some(video),
        audio: Some(audio),
        l2_backend: net.l2_backend(),
    });
    let nic = net.adapter().map(str::to_string);
    let extras = crate::hostinfo::host_extras(Arc::clone(&stop));
    eprintln!(
        "etherquic-play: discovery on UDP 4440, play UDP {udp_port}, service {}",
        host.service_id
    );
    let mut handles = Vec::new();
    for udp in [false, true] {
        let nic = nic.clone();
        let card = Arc::clone(&card);
        let extras = Arc::clone(&extras);
        let stop = Arc::clone(&stop);
        let name = if udp {
            "etherquic-discover-udp"
        } else {
            "etherquic-discover"
        };
        match thread::Builder::new().name(name.into()).spawn(move || {
            let nic = nic.as_deref();
            let served = if udp {
                etherquic::discover::serve_udp(nic, &card, &*extras, &stop)
            } else {
                etherquic::discover::serve_with(nic, &card, &*extras, &stop)
            };
            if let Err(err) = served {
                let kind = if udp { "udp discovery" } else { "discovery" };
                eprintln!("etherquic-play: {kind}: {err}");
            }
        }) {
            Ok(handle) => handles.push(handle),
            Err(err) => eprintln!("etherquic-play: discovery thread: {err}"),
        }
    }
    handles
}

fn computer_name() -> String {
    if let Ok(name) = std::env::var("COMPUTERNAME") {
        let name = name.trim();
        if !name.is_empty() {
            return name.to_string();
        }
    }
    std::fs::read_to_string("/proc/sys/kernel/hostname")
        .ok()
        .map(|text| text.trim().to_string())
        .filter(|text| !text.is_empty())
        .or_else(|| {
            std::fs::read_to_string("/etc/hostname")
                .ok()
                .map(|text| text.trim().to_string())
                .filter(|text| !text.is_empty())
        })
        .unwrap_or_else(|| "etherquic".into())
}

fn join_discovery(handles: Vec<JoinHandle<()>>) {
    for handle in handles {
        join_or_detach(Some(handle));
    }
}

fn subscribe_audio(tx: &broadcast::Sender<AudioChunk>) -> mpsc::Receiver<AudioChunk> {
    let mut rx = tx.subscribe();
    let (out_tx, out_rx) = mpsc::channel(8);
    tokio::spawn(async move {
        loop {
            match rx.recv().await {
                Ok(chunk) => {
                    if out_tx.send(chunk).await.is_err() {
                        break;
                    }
                }
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    });
    out_rx
}

fn subscribe_acks(
    tx: &broadcast::Sender<crate::ack::InputAck>,
) -> mpsc::Receiver<crate::ack::InputAck> {
    let mut rx = tx.subscribe();
    let (out_tx, out_rx) = mpsc::channel(64);
    tokio::spawn(async move {
        loop {
            match rx.recv().await {
                Ok(ack) => {
                    if out_tx.send(ack).await.is_err() {
                        break;
                    }
                }
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    });
    out_rx
}

fn release_host(
    stop: &AtomicBool,
    up: &AtomicBool,
    peers: &AtomicUsize,
    video: &etherquic_media::host::FrameSlot,
    capture: &mut Option<JoinHandle<()>>,
    audio: &mut Option<JoinHandle<()>>,
) {
    video.close();
    stop.store(true, Ordering::SeqCst);
    up.store(false, Ordering::SeqCst);
    peers.store(0, Ordering::SeqCst);
    join_or_detach(capture.take());
    join_or_detach(audio.take());
}

fn join_or_detach(handle: Option<JoinHandle<()>>) {
    let Some(handle) = handle else {
        return;
    };
    let (tx, rx) = std::sync::mpsc::channel();
    let _ = thread::Builder::new()
        .name("etherquic-play-join".into())
        .spawn(move || {
            let _ = handle.join();
            let _ = tx.send(());
        });
    let _ = rx.recv_timeout(Duration::from_secs(2));
}

enum HostWake<T> {
    Conn(T),
    Tick,
    Stop,
}

/// Encode the newest capture once the RTP hello names a carrier. No Quinn socket.
fn offer_carrier_frame(
    frames: &etherquic_media::host::FrameSlot,
    audio: &mut broadcast::Receiver<AudioChunk>,
    served: &mut bool,
) {
    match etherquic::carrier::offered_carrier() {
        Some(id) if !*served => {
            let Some(captured) = frames.latest() else {
                return;
            };
            *served = true;
            let frame = etherquic_media::host::fit_width(&captured, 320);
            let pcm = pull_carrier_pcm(audio);
            std::thread::spawn(move || {
                let _ = etherquic_media::host::serve_carrier(id, &frame, &pcm);
            });
        }
        None => *served = false,
        Some(_) => {}
    }
}

fn pull_carrier_pcm(audio: &mut broadcast::Receiver<AudioChunk>) -> etherquic_media::Pcm {
    let mut latest = None;
    loop {
        match audio.try_recv() {
            Ok(chunk) => latest = Some(chunk),
            Err(broadcast::error::TryRecvError::Lagged(_)) => continue,
            Err(_) => break,
        }
    }
    if let Some(chunk) = latest {
        if !chunk.pcm.is_empty() {
            let start = chunk.pcm.len().saturating_sub(480);
            let samples = chunk.pcm[start..].to_vec();
            if !samples.is_empty() {
                return etherquic_media::Pcm {
                    sample_rate: chunk.sample_rate.max(1),
                    channels: chunk.channels.max(1),
                    samples,
                };
            }
        }
    }
    etherquic_media::Pcm {
        sample_rate: 48_000,
        channels: 1,
        samples: vec![0; 480],
    }
}

async fn until_stop(stop: &AtomicBool) {
    while !stop.load(Ordering::SeqCst) {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

struct Screen {
    width: AtomicU32,
    height: AtomicU32,
    frame_w: AtomicU32,
    frame_h: AtomicU32,
    origin_x: AtomicI32,
    origin_y: AtomicI32,
}

fn capture_loop(
    stop: Arc<AtomicBool>,
    screen: Arc<Screen>,
    video: etherquic_media::host::FrameSlot,
    cursor_tx: tokio::sync::watch::Sender<Option<Vec<u8>>>,
    max_width: u32,
    fps: u32,
) {
    let mut cursor = etherquic::cursor::Watch::new();
    let mut frames = 0u64;
    let mut logged = Instant::now();
    while !stop.load(Ordering::Relaxed) {
        let period = etherquic_media::host::capture_period(fps);
        let started = Instant::now();
        match one_frame(&screen, max_width) {
            Ok(frame) => {
                frames += 1;
                if logged.elapsed() >= Duration::from_secs(10) {
                    eprintln!(
                        "etherquic-play: {frames} frames captured, last {}x{}",
                        frame.width, frame.height
                    );
                    logged = Instant::now();
                }
                if video.is_closed() {
                    break;
                }
                video.put(frame);
            }
            Err(error) => {
                eprintln!("etherquic-play: capture: {error}");
                thread::sleep(Duration::from_secs(1));
                continue;
            }
        }
        let screen = &screen;
        let cursor_tx = &cursor_tx;
        pace(started, period, &stop, || {
            publish_cursor(&mut cursor, screen, cursor_tx)
        });
    }
}

fn publish_cursor(
    cursor: &mut etherquic::cursor::Watch,
    screen: &Screen,
    cursor_tx: &tokio::sync::watch::Sender<Option<Vec<u8>>>,
) {
    let frame_w = screen.frame_w.load(Ordering::Relaxed);
    let frame_h = screen.frame_h.load(Ordering::Relaxed);
    let capture_w = screen.width.load(Ordering::Relaxed);
    let capture_h = screen.height.load(Ordering::Relaxed);
    if frame_w == 0 || frame_h == 0 || capture_w == 0 || capture_h == 0 {
        return;
    }
    let Some(update) = cursor.poll() else {
        return;
    };
    let bytes = update
        .map_to_frame(
            screen.origin_x.load(Ordering::Relaxed),
            screen.origin_y.load(Ordering::Relaxed),
            capture_w,
            capture_h,
            frame_w,
            frame_h,
        )
        .encode();
    let _ = cursor_tx.send(Some(bytes));
}

fn pace(started: Instant, period: Duration, stop: &AtomicBool, mut on_tick: impl FnMut()) {
    loop {
        if stop.load(Ordering::Relaxed) {
            return;
        }
        let left = period.saturating_sub(started.elapsed());
        on_tick();
        if left.is_zero() {
            return;
        }
        thread::sleep(left.min(Duration::from_millis(16)));
    }
}

fn one_frame(screen: &Screen, max_width: u32) -> Result<etherquic_media::Frame, String> {
    let monitors = Monitor::all().map_err(|error| error.to_string())?;
    let index = monitors
        .iter()
        .position(|monitor| monitor.is_primary().unwrap_or(false))
        .unwrap_or(0);
    let monitor = monitors.get(index).ok_or("no monitor")?;
    let width = monitor.width().map_err(|error| error.to_string())?;
    let height = monitor.height().map_err(|error| error.to_string())?;
    screen.width.store(width.max(1), Ordering::Relaxed);
    screen.height.store(height.max(1), Ordering::Relaxed);
    screen
        .origin_x
        .store(monitor.x().unwrap_or(0), Ordering::Relaxed);
    screen
        .origin_y
        .store(monitor.y().unwrap_or(0), Ordering::Relaxed);
    let image = monitor.capture_image().map_err(|error| error.to_string())?;
    let picture = fit_rgba(image.as_raw(), image.width(), image.height(), max_width)
        .map_err(|error| error.to_string())?;
    screen.frame_w.store(picture.width, Ordering::Relaxed);
    screen.frame_h.store(picture.height, Ordering::Relaxed);
    Ok(etherquic_media::Frame {
        width: picture.width,
        height: picture.height,
        rgb: picture.rgb,
    })
}

fn capture_audio(
    stop: Arc<AtomicBool>,
    audio: broadcast::Sender<AudioChunk>,
    wanted: Option<&str>,
) -> Result<(), String> {
    let host = cpal::default_host();
    let (device, config) = match wanted.and_then(|wanted| named_source(&host, wanted)) {
        Some(found) => found,
        None => desktop_stream(&host)?,
    };
    let name = device.name().unwrap_or_else(|_| "audio".into());
    eprintln!("etherquic-play: capturing audio from {name}");
    let rate = config.sample_rate().0;
    let channels = u16::try_from(config.channels()).map_err(|_| "too many audio channels")?;
    let pending = Arc::new(Mutex::new(Vec::<i16>::new()));
    let stream_pending = pending.clone();
    let stream = match config.sample_format() {
        SampleFormat::F32 => {
            input_stream(&device, &config, stream_pending, channels, |sample: f32| {
                sample
            })?
        }
        SampleFormat::I16 => {
            input_stream(&device, &config, stream_pending, channels, |sample: i16| {
                sample.to_float_sample()
            })?
        }
        SampleFormat::U16 => {
            input_stream(&device, &config, stream_pending, channels, |sample: u16| {
                sample.to_float_sample()
            })?
        }
        other => return Err(format!("unsupported input format {other}")),
    };
    stream.play().map_err(|error| error.to_string())?;
    while !stop.load(Ordering::Relaxed) {
        thread::sleep(Duration::from_millis(20));
        let samples = {
            let mut guard = pending.lock().unwrap_or_else(|poison| poison.into_inner());
            std::mem::take(&mut *guard)
        };
        if samples.is_empty() {
            continue;
        }
        let chunk = AudioChunk {
            sample_rate: rate,
            channels,
            pcm: samples,
        };
        let _ = audio.send(chunk);
    }
    Ok(())
}

fn desktop_stream(
    host: &cpal::Host,
) -> Result<(cpal::Device, cpal::SupportedStreamConfig), String> {
    let (monitor, first) = input_devices(host);
    let output_available = host.default_output_device().is_some();
    match desktop_audio(cfg!(windows), output_available, monitor.is_some()) {
        DesktopAudio::DefaultOutput => {
            // Speakers are eRender. build_input_stream on that device is the WASAPI loopback.
            let device = host
                .default_output_device()
                .ok_or("no audio output device")?;
            let config = device
                .default_output_config()
                .map_err(|error| error.to_string())?;
            Ok((device, config))
        }
        DesktopAudio::MonitorInput => {
            let device = monitor.ok_or("no monitor input")?;
            let config = device
                .default_input_config()
                .map_err(|error| error.to_string())?;
            Ok((device, config))
        }
        DesktopAudio::CaptureInput => {
            let device = first
                .or_else(|| host.default_input_device())
                .ok_or("no audio input device")?;
            let config = device
                .default_input_config()
                .map_err(|error| error.to_string())?;
            Ok((device, config))
        }
    }
}

/// A capture device by name, or an output by name opened as loopback on Windows.
fn named_source(
    host: &cpal::Host,
    wanted: &str,
) -> Option<(cpal::Device, cpal::SupportedStreamConfig)> {
    let named = |device: &cpal::Device| device.name().map(|name| name == wanted).unwrap_or(false);
    if let Some(device) = host.input_devices().ok()?.find(named) {
        let config = device.default_input_config().ok()?;
        return Some((device, config));
    }
    if cfg!(windows) {
        if let Some(device) = host.output_devices().ok()?.find(named) {
            let config = device.default_output_config().ok()?;
            return Some((device, config));
        }
    }
    None
}

/// Names of this computer's audio outputs and inputs, defaults first.
pub fn audio_devices() -> (Vec<String>, Vec<String>) {
    let host = cpal::default_host();
    let list = |devices: Option<Vec<cpal::Device>>, default: Option<cpal::Device>| {
        let default = default.and_then(|device| device.name().ok());
        let mut names: Vec<String> = devices
            .unwrap_or_default()
            .iter()
            .filter_map(|device| device.name().ok())
            .collect();
        names.sort();
        names.dedup();
        if let Some(default) = default {
            names.retain(|name| *name != default);
            names.insert(0, default);
        }
        names
    };
    let outputs = list(
        host.output_devices().ok().map(Iterator::collect),
        host.default_output_device(),
    );
    let inputs = list(
        host.input_devices().ok().map(Iterator::collect),
        host.default_input_device(),
    );
    (outputs, inputs)
}

fn input_devices(host: &cpal::Host) -> (Option<cpal::Device>, Option<cpal::Device>) {
    let mut monitor = None;
    let mut first = None;
    let Ok(devices) = host.input_devices() else {
        return (None, None);
    };
    for device in devices {
        let name = device.name().unwrap_or_default().to_ascii_lowercase();
        if monitor.is_none() && (name.contains("monitor") || name.contains("loopback")) {
            monitor = Some(device);
            continue;
        }
        if first.is_none() {
            first = Some(device);
        }
    }
    (monitor, first)
}

fn input_stream<T, F>(
    device: &cpal::Device,
    config: &cpal::SupportedStreamConfig,
    pending: Arc<Mutex<Vec<i16>>>,
    channels: u16,
    to_float: F,
) -> Result<cpal::Stream, String>
where
    T: Sample + cpal::SizedSample + Send + 'static,
    F: Fn(T) -> f32 + Send + 'static,
{
    device
        .build_input_stream(
            &config.config(),
            move |data: &[T], _| push_input(&pending, data, channels, &to_float),
            |error| eprintln!("etherquic-play: audio: {error}"),
            None,
        )
        .map_err(|error| error.to_string())
}

fn push_input<T, F>(pending: &Mutex<Vec<i16>>, data: &[T], channels: u16, to_float: &F)
where
    F: Fn(T) -> f32,
    T: Copy,
{
    let mut guard = match pending.try_lock() {
        Ok(guard) => guard,
        Err(_) => return,
    };
    for sample in data {
        guard.push(sample_to_i16(to_float(*sample)));
    }
    let cap = 48_000usize * usize::from(channels.max(1));
    if guard.len() > cap {
        let drop_n = guard.len() - cap;
        guard.drain(..drop_n);
    }
}

fn sample_to_i16(sample: f32) -> i16 {
    let float = sample.clamp(-1.0, 1.0);
    (float * i16::MAX as f32) as i16
}

static INPUT_RECORDER: Mutex<Option<Arc<Mutex<Vec<Event>>>>> = Mutex::new(None);

/// Record injected input here instead of typing on this desktop.
///
/// `None` restores the real keyboard and mouse. The desktop session still
/// calls the same `Hands::one` path.
pub fn set_input_recorder(slot: Option<Arc<Mutex<Vec<Event>>>>) {
    *INPUT_RECORDER
        .lock()
        .unwrap_or_else(|poison| poison.into_inner()) = slot;
}

fn recorded_input() -> Option<Arc<Mutex<Vec<Event>>>> {
    INPUT_RECORDER
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .clone()
}

struct Hands {
    enigo: Option<Enigo>,
    #[cfg(target_os = "linux")]
    wayland: Option<crate::seat_wl::PointerSeat>,
    #[cfg(target_os = "linux")]
    ei: Option<crate::seat_ei::EiSeat>,
    pads: Pads,
    buttons: u8,
    warned: bool,
}

impl Hands {
    fn open() -> Self {
        #[cfg(target_os = "linux")]
        let wayland = open_wayland();
        #[cfg(target_os = "linux")]
        let ei = if wayland.is_none() { open_ei() } else { None };
        let enigo = match Enigo::new(&Settings::default()) {
            Ok(enigo) => Some(enigo),
            Err(error) => {
                let kind = if cfg!(target_os = "linux") {
                    "keyboard"
                } else {
                    "keyboard and mouse"
                };
                eprintln!("etherquic-play: {kind} injection is unavailable: {error}");
                None
            }
        };
        Self {
            enigo,
            #[cfg(target_os = "linux")]
            wayland,
            #[cfg(target_os = "linux")]
            ei,
            pads: Pads::open(),
            buttons: 0,
            warned: false,
        }
    }

    fn apply(
        &mut self,
        events: &[Event],
        width: u32,
        height: u32,
        acks: &mpsc::Sender<crate::ack::InputAck>,
    ) {
        for event in events {
            let outcome = self.one(event, width, height);
            if let Some(ack) = crate::ack::InputAck::confirm(event, &outcome) {
                let _ = acks.try_send(ack);
            }
            if let Err(error) = outcome {
                if !self.warned {
                    eprintln!("etherquic-play: inject: {error}");
                    self.warned = true;
                }
            }
        }
    }

    fn one(&mut self, event: &Event, width: u32, height: u32) -> Result<bool, String> {
        if let Some(slot) = recorded_input() {
            slot.lock()
                .unwrap_or_else(|poison| poison.into_inner())
                .push(event.clone());
            return Ok(true);
        }
        match &event.body {
            Body::Pointer {
                x,
                y,
                buttons,
                wheel_y,
                wheel_x,
            } => {
                // Screen extent, not u32::MAX. A virtual pointer maps position/extent.
                let place = place_pointer(*x, *y, width, height);
                self.move_abs(place)?;
                self.buttons(*buttons)?;
                self.wheels(*wheel_y, *wheel_x)?;
            }
            Body::Delta {
                dx,
                dy,
                buttons,
                wheel_y,
                wheel_x,
            } => {
                self.move_rel(i32::from(*dx), i32::from(*dy))?;
                self.buttons(*buttons)?;
                self.wheels(*wheel_y, *wheel_x)?;
            }
            Body::Key { down, source, code } => {
                if *source != KeySource::Keysym {
                    return Ok(false);
                }
                let Some(key) = key_from_keysym(*code) else {
                    return Ok(false);
                };
                let direction = if *down {
                    Direction::Press
                } else {
                    Direction::Release
                };
                self.seat()?
                    .key(key, direction)
                    .map_err(|error| error.to_string())?;
            }
            Body::Gamepad {
                index,
                buttons,
                axes,
            } => {
                self.pads.apply(*index, pad_report(*buttons, *axes))?;
            }
            Body::Sync { .. } | Body::Unknown { .. } => return Ok(false),
        }
        Ok(true)
    }

    fn seat(&mut self) -> Result<&mut Enigo, String> {
        self.enigo
            .as_mut()
            .ok_or_else(|| "keyboard and mouse injection is unavailable".into())
    }

    fn move_abs(&mut self, place: PointerPlace) -> Result<(), String> {
        #[cfg(target_os = "linux")]
        if let Some(seat) = self.wayland.as_mut() {
            return seat.absolute(place);
        }
        #[cfg(target_os = "linux")]
        if let Some(seat) = self.ei.as_mut() {
            return seat.absolute(place);
        }
        let x = i32::try_from(place.x).map_err(|_| "pointer x is off the screen")?;
        let y = i32::try_from(place.y).map_err(|_| "pointer y is off the screen")?;
        self.seat()?
            .move_mouse(x, y, Coordinate::Abs)
            .map_err(|error| error.to_string())
    }

    fn move_rel(&mut self, dx: i32, dy: i32) -> Result<(), String> {
        #[cfg(target_os = "linux")]
        if let Some(seat) = self.wayland.as_mut() {
            return seat.relative(dx, dy);
        }
        #[cfg(target_os = "linux")]
        if let Some(seat) = self.ei.as_mut() {
            return seat.relative(dx, dy);
        }
        self.seat()?
            .move_mouse(dx, dy, Coordinate::Rel)
            .map_err(|error| error.to_string())
    }

    fn wheels(&mut self, wheel_y: i16, wheel_x: i16) -> Result<(), String> {
        if wheel_y == 0 && wheel_x == 0 {
            return Ok(());
        }
        #[cfg(target_os = "linux")]
        if let Some(seat) = self.wayland.as_mut() {
            return seat.wheel(wheel_y, wheel_x);
        }
        #[cfg(target_os = "linux")]
        if let Some(seat) = self.ei.as_mut() {
            return seat.wheel(wheel_y, wheel_x);
        }
        if wheel_y != 0 {
            self.seat()?
                .scroll(i32::from(wheel_y), Axis::Vertical)
                .map_err(|error| error.to_string())?;
        }
        if wheel_x != 0 {
            self.seat()?
                .scroll(i32::from(wheel_x), Axis::Horizontal)
                .map_err(|error| error.to_string())?;
        }
        Ok(())
    }

    fn buttons(&mut self, next: u8) -> Result<(), String> {
        for (bit, button) in [
            (Buttons::LEFT, Button::Left),
            (Buttons::MIDDLE, Button::Middle),
            (Buttons::RIGHT, Button::Right),
        ] {
            let was = self.buttons & bit != 0;
            let now = next & bit != 0;
            if was == now {
                continue;
            }
            #[cfg(target_os = "linux")]
            if let Some(code) = crate::seat_wl::button_code(bit) {
                if let Some(seat) = self.wayland.as_mut() {
                    seat.button(code, now)?;
                    continue;
                }
                if let Some(seat) = self.ei.as_mut() {
                    seat.button(code, now)?;
                    continue;
                }
            }
            let direction = if now {
                Direction::Press
            } else {
                Direction::Release
            };
            self.seat()?
                .button(button, direction)
                .map_err(|error| error.to_string())?;
        }
        self.buttons = next;
        Ok(())
    }
}

#[cfg(target_os = "linux")]
fn open_wayland() -> Option<crate::seat_wl::PointerSeat> {
    match crate::seat_wl::PointerSeat::open() {
        Ok(seat) => Some(seat),
        Err(error) => {
            eprintln!("etherquic-play: wayland pointer is unavailable: {error}");
            None
        }
    }
}

#[cfg(target_os = "linux")]
fn open_ei() -> Option<crate::seat_ei::EiSeat> {
    eprintln!("etherquic-play: opening a libei pointer");
    match crate::seat_ei::EiSeat::open() {
        Ok(seat) => Some(seat),
        Err(error) => {
            eprintln!("etherquic-play: libei pointer is unavailable: {error}");
            None
        }
    }
}

/// Linux xdotool can emit KP_Left as a keysym. Windows and macOS enigo have
/// no keypad-left variant, so those hosts fall back to the dedicated key.
fn keypad_key(dedicated: Key, keysym: u32) -> Key {
    #[cfg(target_os = "linux")]
    {
        let _ = dedicated;
        Key::Other(keysym)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = keysym;
        dedicated
    }
}

fn key_from_keysym(code: u32) -> Option<Key> {
    Some(match code {
        0xff1b => Key::Escape,
        0xff0d | 0xff8d => Key::Return,
        0xff08 => Key::Backspace,
        0xff09 => Key::Tab,
        0xffff => Key::Delete,
        0xff9f => keypad_key(Key::Delete, 0xff9f),
        #[cfg(not(target_os = "macos"))]
        0xff63 => Key::Insert,
        #[cfg(not(target_os = "macos"))]
        0xff9e => keypad_key(Key::Insert, 0xff9e),
        #[cfg(target_os = "macos")]
        0xff63 | 0xff9e => Key::Help,
        0xff51 => Key::LeftArrow,
        0xff96 => keypad_key(Key::LeftArrow, 0xff96),
        0xff52 => Key::UpArrow,
        0xff97 => keypad_key(Key::UpArrow, 0xff97),
        0xff53 => Key::RightArrow,
        0xff98 => keypad_key(Key::RightArrow, 0xff98),
        0xff54 => Key::DownArrow,
        0xff99 => keypad_key(Key::DownArrow, 0xff99),
        0xff50 => Key::Home,
        0xff95 => keypad_key(Key::Home, 0xff95),
        0xff57 => Key::End,
        0xff9c => keypad_key(Key::End, 0xff9c),
        0xff55 => Key::PageUp,
        0xff9a => keypad_key(Key::PageUp, 0xff9a),
        0xff56 => Key::PageDown,
        0xff9b => keypad_key(Key::PageDown, 0xff9b),
        0xffb0..=0xffb9 => Key::Unicode(char::from(b'0' + (code - 0xffb0) as u8)),
        0xffe1 | 0xffe2 => Key::Shift,
        0xffe3 | 0xffe4 => Key::Control,
        0xffe9 | 0xffea => Key::Alt,
        0xffeb | 0xffec => Key::Meta,
        0xffbe => Key::F1,
        0xffbf => Key::F2,
        0xffc0 => Key::F3,
        0xffc1 => Key::F4,
        0xffc2 => Key::F5,
        0xffc3 => Key::F6,
        0xffc4 => Key::F7,
        0xffc5 => Key::F8,
        0xffc6 => Key::F9,
        0xffc7 => Key::F10,
        0xffc8 => Key::F11,
        0xffc9 => Key::F12,
        code => {
            let ch = char::from_u32(code)?;
            if ch.is_control() {
                return None;
            }
            Key::Unicode(ch)
        }
    })
}

#[cfg(test)]
mod tests {
    use super::key_from_keysym;
    use enigo::Key;

    #[test]
    fn typed_paste_covers_letters_past_ascii() {
        assert!(matches!(
            key_from_keysym(u32::from('a')),
            Some(Key::Unicode('a'))
        ));
        assert!(matches!(
            key_from_keysym(u32::from('é')),
            Some(Key::Unicode('é'))
        ));
        assert!(matches!(key_from_keysym(0xff0d), Some(Key::Return)));
        assert!(key_from_keysym(u32::from('\n')).is_none());
        assert!(matches!(key_from_keysym(0xff51), Some(Key::LeftArrow)));
        assert!(matches!(key_from_keysym(0xff54), Some(Key::DownArrow)));
        assert!(!matches!(key_from_keysym(0xff51), Some(Key::Unicode(_))));
        assert_eq!(key_from_keysym(0xffb4), Some(Key::Unicode('4')));
        #[cfg(target_os = "linux")]
        assert!(matches!(key_from_keysym(0xff96), Some(Key::Other(0xff96))));
        #[cfg(not(target_os = "linux"))]
        assert!(matches!(key_from_keysym(0xff96), Some(Key::LeftArrow)));
    }
}
