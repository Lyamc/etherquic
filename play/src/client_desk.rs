//! Player window. Focus it to send the keyboard and the mouse.
//! Gamepads are read on their own thread for as long as the view is open.

use std::collections::{HashMap, VecDeque};
use std::net::SocketAddr;
use std::num::NonZeroU32;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, SampleFormat, SizedSample};

use etherquic::{
    install_pure_crypto, l2_client_endpoint, peer_to_socket_addr, udp_client_endpoint_on, L2Net,
};
use etherquic_input::{Body, Buttons, Event, KeySource};
use softbuffer::{Context, Surface};
use tokio::sync::mpsc;
use winit::application::ApplicationHandler;
use winit::dpi::PhysicalSize;
use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy};
use winit::keyboard::{Key, KeyCode, ModifiersState, NamedKey, PhysicalKey};
use winit::platform::run_on_demand::EventLoopExtRunOnDemand;
use winit::window::{CursorIcon, CustomCursor, Icon, Window, WindowId};

use crate::ack::TraceClock;
use crate::desk_io::{self, NoteSide};
use crate::outbox::Gate;
use crate::picture_hub::{self, PictureSeat};
use crate::{
    blit_rgb, format_mac, play_session, resample, session_end_status, stroke_rgb, AudioChunk,
    Picture, Status,
};

/// Keystroke strings waiting for the picture window. Drained on the window
/// thread. Sending does not require the window to be focused.
static PENDING_KEYSTROKES: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// Queue a string of keys for the open picture window.
///
/// The focused keyboard path still drops keys while the window is in the
/// background. This queue does not.
pub fn queue_keystroke_string(text: &str) {
    if let Some(id) = picture_hub::shown_picture() {
        if desk_io::send_keys(id, text) {
            return;
        }
    }
    if let Ok(mut queue) = PENDING_KEYSTROKES.lock() {
        queue.push(text.to_string());
    }
}

/// Queue keystrokes for the window thread, which releases held modifiers first.
///
/// False when no picture is on screen, and nothing is queued.
pub fn queue_window_keystrokes(text: &str) -> bool {
    if text.is_empty() || picture_hub::shown_picture().is_none() {
        return false;
    }
    if let Ok(mut queue) = PENDING_KEYSTROKES.lock() {
        queue.push(text.to_string());
    } else {
        return false;
    }
    picture_hub::wake_picture();
    true
}

/// What `connect` dials. The NIC is already open when `udp` is absent.
pub struct ConnectSession {
    pub local_mac: [u8; 6],
    pub peer_mac: [u8; 6],
    pub service_id: u16,
    /// Ask the host to confirm stamped input and show how long that took.
    pub trace_input: bool,
    /// Dial this UDP address instead of the L2 service.
    pub udp: Option<SocketAddr>,
    /// Scale, what to send, pad slots, and the speaker. Read while the view runs.
    pub view: crate::LiveView,
    /// Codec order, pins and limits for the offer.
    pub codecs: crate::CodecChoice,
    /// Set to this session's picture id before the view blocks.
    /// The window uses it to switch which desktop is on screen.
    pub picture: Arc<AtomicU64>,
}

/// Wayland only allows a winit loop on the main thread unless the builder opts in.
/// The UI already runs GPUI on the main thread, so the picture window has its own thread.
///
/// winit permits one [`EventLoop`] per process and `run_app` consumes it. This
/// builder runs on the picture-window thread, and each connect calls
/// [`EventLoopExtRunOnDemand::run_app_on_demand`] on that same loop.
#[cfg_attr(target_os = "macos", allow(dead_code))]
fn player_event_loop() -> Result<EventLoop<()>, winit::error::EventLoopError> {
    let mut builder = EventLoop::builder();
    #[cfg(target_os = "linux")]
    {
        use winit::platform::wayland::EventLoopBuilderExtWayland;
        builder.with_any_thread(true);
    }
    #[cfg(target_os = "windows")]
    {
        use winit::platform::windows::EventLoopBuilderExtWindows;
        builder.with_any_thread(true);
    }
    builder.build()
}

struct ViewRequest {
    frames: Arc<Mutex<Option<Picture>>>,
    input: Arc<Gate>,
    stop: Arc<AtomicBool>,
    trace: bool,
    view: crate::LiveView,
    stamps: Arc<Mutex<HashMap<u64, Instant>>>,
    title: Arc<Mutex<String>>,
    cursor: Arc<Mutex<etherquic::cursor::CursorView>>,
    proxy: std::sync::mpsc::Sender<Result<EventLoopProxy<()>, String>>,
    done: std::sync::mpsc::Sender<Result<(), String>>,
}

#[cfg_attr(target_os = "macos", allow(dead_code))]
static WINDOW_PROXY: Mutex<Option<EventLoopProxy<()>>> = Mutex::new(None);

#[cfg_attr(target_os = "macos", allow(dead_code))]
fn window_proxy() -> Option<EventLoopProxy<()>> {
    WINDOW_PROXY
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .clone()
}

#[cfg_attr(target_os = "macos", allow(dead_code))]
fn ensure_window_thread() {
    static STARTED: OnceLock<()> = OnceLock::new();
    STARTED.get_or_init(|| {
        let _ = thread::Builder::new()
            .name("etherquic-play-window".into())
            .spawn(window_thread);
    });
}

#[cfg_attr(target_os = "macos", allow(dead_code))]
fn window_thread() {
    let mut event_loop = match player_event_loop() {
        Ok(event_loop) => event_loop,
        Err(err) => {
            eprintln!("etherquic-play: picture window: {err}");
            return;
        }
    };
    let proxy = event_loop.create_proxy();
    {
        let wake = proxy.clone();
        picture_hub::set_picture_waker(Box::new(move || {
            let _ = wake.send_event(());
        }));
        *WINDOW_PROXY
            .lock()
            .unwrap_or_else(|poison| poison.into_inner()) = Some(proxy);
    }
    loop {
        picture_hub::wait_for_picture();
        let mut app = App::for_hub();
        if !app.bind_shown() {
            continue;
        }
        let _ = event_loop.run_app_on_demand(&mut app);
        app.drop_window();
    }
}

/// Run one picture window on `event_loop` until it closes, and report on `done`.
fn run_view(event_loop: &mut EventLoop<()>, request: ViewRequest) {
    {
        let proxy = event_loop.create_proxy();
        if request.proxy.send(Ok(proxy)).is_err() {
            let _ = request.done.send(Err("the picture window stopped".into()));
            return;
        }
        let mut app = App {
            frames: request.frames,
            input: request.input,
            stop: request.stop,
            window: None,
            context: None,
            surface: None,
            picture: None,
            debug_prev: None,
            debug_tiles: Vec::new(),
            debug_on: false,
            buttons: 0,
            focused: false,
            trace: request.trace,
            clock: TraceClock::default(),
            opened: Instant::now(),
            stamps: request.stamps,
            title: request.title,
            remote_cursor: request.cursor,
            cursor_mode: crate::CursorMode::Native,
            cursor_seq: 0,
            cursor_visible: true,
            cursor_icon: None,
            cursor_image: 0,
            shortcuts: None,
            modifiers: ModifiersState::empty(),
            cursor: (0.0, 0.0),
            letterbox: crate::place(crate::ScaleMode::Fit, 0, 0, 1, 1),
            view: request.view,
            fitted: (0, 0),
            hub: false,
            bound_id: 0,
            phase: Phase::Idle,
        };
        let result = event_loop
            .run_app_on_demand(&mut app)
            .map_err(|err| err.to_string());
        app.drop_window();
        let _ = request.done.send(result);
    }
}

/// macOS runs a winit loop on the main thread only. The command-line player
/// calls [`connect_view`] there, so its window runs inline on one loop kept
/// for the process. A caller on another thread gets an error, not a panic.
#[cfg(target_os = "macos")]
fn on_main_thread() -> bool {
    // Rust names the thread that runs `main` "main". The crate forbids unsafe,
    // so this stands in for pthread_main_np.
    std::thread::current().name() == Some("main")
}

#[cfg(target_os = "macos")]
fn run_view_here(request: ViewRequest) {
    thread_local! {
        static LOOP: std::cell::RefCell<Option<EventLoop<()>>> = const { std::cell::RefCell::new(None) };
    }
    LOOP.with(|slot| {
        let mut slot = slot.borrow_mut();
        if slot.is_none() {
            match EventLoop::builder().build() {
                Ok(event_loop) => *slot = Some(event_loop),
                Err(err) => {
                    let _ = request.proxy.send(Err(err.to_string()));
                    let _ = request.done.send(Err(err.to_string()));
                    return;
                }
            }
        }
        if let Some(event_loop) = slot.as_mut() {
            run_view(event_loop, request);
        }
    });
}

/// Wake the window when a picture or a title arrives, and close it on Stop.
async fn wake_window(
    proxy: EventLoopProxy<()>,
    frames: Arc<Mutex<Option<Picture>>>,
    stop: Arc<AtomicBool>,
    pads_stop: Arc<AtomicBool>,
    title: Arc<Mutex<String>>,
    cursor: Arc<Mutex<etherquic::cursor::CursorView>>,
) {
    let mut seen = 0u64;
    loop {
        tokio::time::sleep(Duration::from_millis(30)).await;
        if stop.load(Ordering::SeqCst) {
            pads_stop.store(true, Ordering::SeqCst);
            let _ = proxy.send_event(());
            break;
        }
        let ready = frames.lock().map(|slot| slot.is_some()).unwrap_or(false);
        let titled = title.lock().map(|slot| !slot.is_empty()).unwrap_or(false);
        let seq = cursor.lock().map(|slot| slot.seq).unwrap_or(seen);
        if ready || titled || seq != seen {
            seen = seq;
            let _ = proxy.send_event(());
        }
    }
}

fn view_error(message: impl std::fmt::Display) -> Box<dyn std::error::Error + Send + Sync> {
    Box::new(std::io::Error::other(message.to_string()))
}

/// Open the remote desktop and block until that window closes or `stop` is set.
///
/// The window shows the host's picture, plays its audio, and sends the keyboard,
/// the mouse, and a local gamepad. Focus the window to send the keyboard and the mouse.
pub fn connect_view(
    net: Option<Arc<L2Net>>,
    connect: ConnectSession,
    stop: Arc<AtomicBool>,
    status: Status,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    #[cfg(target_os = "macos")]
    if !on_main_thread() {
        return Err(view_error(
            "On macOS the picture window has to run on the main thread, which this window already uses. Use etherquic-play connect from a terminal for now.",
        ));
    }
    install_pure_crypto();
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    let frames = Arc::new(Mutex::new(None::<Picture>));
    let cursors = etherquic::cursor::share();
    let (audio_tx, audio_rx) = std::sync::mpsc::channel::<AudioChunk>();
    let playback = thread::Builder::new()
        .name("etherquic-play-playback".into())
        .spawn({
            let output = crate::snapshot(&connect.view).output_device;
            move || play_audio(audio_rx, output)
        })?;
    // One slot. The gate keeps the latest pointer and every key, and keys leave first.
    let (input_tx, input_rx) = mpsc::channel::<Vec<Event>>(32);
    let key_input = input_tx.clone();
    let gate = Arc::new(Gate::new());
    let forward_gate = Arc::clone(&gate);
    rt.spawn(async move {
        loop {
            forward_gate.wait().await;
            while let Some(event) = forward_gate.pop() {
                if input_tx.send(vec![event]).await.is_err() {
                    return;
                }
            }
        }
    });
    // Diagnostics: ETHERQUIC_PROBE_INPUT=1 moves the host pointer in a small
    // circle a few seconds in, so a headless test sees input arrive (with
    // --trace-input the host confirms each move).
    if std::env::var_os("ETHERQUIC_PROBE_INPUT").is_some() {
        let probe_gate = Arc::clone(&gate);
        let _ = thread::Builder::new()
            .name("etherquic-probe-input".into())
            .spawn(move || {
                thread::sleep(Duration::from_secs(4));
                for step in 0..40u32 {
                    let angle = step as f32 * 0.3;
                    probe_gate.push(Event {
                        time_us: 1_000 + u64::from(step) * 1_000,
                        body: Body::Pointer {
                            x: (32768.0 + 6000.0 * angle.cos()) as u16,
                            y: (32768.0 + 6000.0 * angle.sin()) as u16,
                            buttons: 0,
                            wheel_y: 0,
                            wheel_x: 0,
                        },
                    });
                    thread::sleep(Duration::from_millis(100));
                }
                eprintln!("etherquic-play: probe input sent 40 pointer moves");
            });
    }
    // Closed with the view, including when the caller sets `stop`.
    let pads_stop = Arc::new(AtomicBool::new(false));
    let pad_flag = Arc::clone(&pads_stop);
    let pad_input = Arc::clone(&gate);
    let pad_view = Arc::clone(&connect.view);
    let window_view = Arc::clone(&connect.view);
    let gamepad = thread::Builder::new()
        .name("etherquic-play-gamepad".into())
        .spawn(move || crate::player_pad::run(pad_input, pad_flag, pad_view))?;
    let net_frames = frames.clone();
    let net_cursors = Arc::clone(&cursors);
    let dial_stop = Arc::clone(&stop);
    let dial_status = Arc::clone(&status);
    let trace_input = connect.trace_input;
    let stamps = Arc::new(Mutex::new(HashMap::<u64, Instant>::new()));
    let title = Arc::new(Mutex::new(String::new()));
    let dial_stamps = Arc::clone(&stamps);
    let dial_title = Arc::clone(&title);
    #[cfg(not(target_os = "macos"))]
    let picture_id = {
        let id = picture_hub::open_picture(
            "player",
            PictureSeat {
                id: 0,
                frames: Arc::clone(&frames),
                input: Arc::clone(&gate),
                stop: Arc::clone(&stop),
                trace: trace_input,
                view: Arc::clone(&window_view),
                stamps: Arc::clone(&stamps),
                title: Arc::clone(&title),
                cursor: Arc::clone(&cursors),
            },
        );
        connect.picture.store(id, Ordering::SeqCst);
        let (key_tx, mut key_rx) = mpsc::unbounded_channel::<String>();
        desk_io::attach_keys(id, key_tx);
        rt.spawn(async move {
            while let Some(text) = key_rx.recv().await {
                if crate::submit_keystrokes(&key_input, &text).await.is_err() {
                    break;
                }
            }
        });
        id
    };
    #[cfg(target_os = "macos")]
    let picture_id = {
        let _ = key_input;
        0u64
    };
    rt.spawn(async move {
        if let Err(error) = connect_player(
            net,
            connect,
            net_frames,
            audio_tx,
            input_rx,
            dial_stop,
            dial_status,
            dial_stamps,
            dial_title,
            picture_id,
            net_cursors,
        )
        .await
        {
            eprintln!("etherquic-play: {error}");
        }
    });

    let (proxy_tx, proxy_rx) = std::sync::mpsc::channel();
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    let request = ViewRequest {
        frames: frames.clone(),
        input: Arc::clone(&gate),
        stop: Arc::clone(&stop),
        trace: trace_input,
        view: window_view,
        stamps: Arc::clone(&stamps),
        title: Arc::clone(&title),
        cursor: Arc::clone(&cursors),
        proxy: proxy_tx,
        done: done_tx,
    };
    let wake_frames = frames.clone();
    let wake_stop = Arc::clone(&stop);
    let wake_pads = Arc::clone(&pads_stop);
    let wake_title = Arc::clone(&title);
    let wake_cursors = Arc::clone(&cursors);
    #[cfg(target_os = "macos")]
    {
        // The window runs on this thread, so the waker picks up the proxy itself.
        rt.spawn(async move {
            let proxy = match tokio::task::spawn_blocking(move || proxy_rx.recv()).await {
                Ok(Ok(Ok(proxy))) => proxy,
                _ => return,
            };
            wake_window(
                proxy,
                wake_frames,
                wake_stop,
                wake_pads,
                wake_title,
                Arc::clone(&wake_cursors),
            )
            .await;
        });
        run_view_here(request);
    }
    #[cfg(not(target_os = "macos"))]
    {
        // The picture window rebinds to whichever seat is shown. This session
        // waits on its own seat, so closing it does not join the window thread
        // while another desktop is on screen.
        let _ = (request, proxy_rx, done_rx);
        ensure_window_thread();
        let picture_stop = Arc::clone(&stop);
        rt.spawn(async move {
            let proxy = loop {
                if picture_stop.load(Ordering::SeqCst) {
                    return;
                }
                if let Some(proxy) = window_proxy() {
                    break proxy;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            };
            wake_window(
                proxy,
                wake_frames,
                wake_stop,
                wake_pads,
                wake_title,
                Arc::clone(&wake_cursors),
            )
            .await;
        });
        picture_hub::hold_picture(picture_id, &stop);
        desk_io::detach_keys(picture_id);
    }
    #[cfg(target_os = "macos")]
    {
        done_rx
            .recv()
            .map_err(|_| view_error("the picture window stopped"))?
            .map_err(view_error)?;
    }
    // Keep the runtime until the window loop returns, then join the side threads.
    drop(rt);
    release_view(pads_stop, playback, gamepad);
    Ok(())
}

fn release_view(pads_stop: Arc<AtomicBool>, playback: JoinHandle<()>, gamepad: JoinHandle<()>) {
    pads_stop.store(true, Ordering::SeqCst);
    let _ = playback.join();
    let _ = gamepad.join();
}

struct DetachPlayer(u64);

impl Drop for DetachPlayer {
    fn drop(&mut self) {
        desk_io::detach_player(self.0);
    }
}

async fn connect_player(
    net: Option<Arc<L2Net>>,
    connect: ConnectSession,
    frames: Arc<Mutex<Option<Picture>>>,
    speaker: std::sync::mpsc::Sender<AudioChunk>,
    input: mpsc::Receiver<Vec<Event>>,
    stop: Arc<AtomicBool>,
    status: Status,
    stamps: Arc<Mutex<HashMap<u64, Instant>>>,
    title: Arc<Mutex<String>>,
    picture_id: u64,
    cursors: Arc<Mutex<etherquic::cursor::CursorView>>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let (cmd_tx, pipes, outbound, inbound) = desk_io::open_notes(desk_io::desk_roster());
    desk_io::attach_player(picture_id, cmd_tx);
    let _detach = DetachPlayer(picture_id);
    tokio::spawn(desk_io::pump_outbound(outbound));
    tokio::spawn(desk_io::pump_inbound(NoteSide::Player, inbound));
    let (endpoint, peer, peer_text) = if let Some(addr) = connect.udp {
        let bind = if addr.is_ipv4() {
            SocketAddr::from(([0, 0, 0, 0], 0))
        } else {
            SocketAddr::from((std::net::Ipv6Addr::UNSPECIFIED, 0))
        };
        let endpoint = udp_client_endpoint_on(bind)?;
        (endpoint, addr, addr.to_string())
    } else {
        let net = net.ok_or("no adapter for the L2 dial")?;
        // One discovery burst first. The host answers from its L2 MAC, so a
        // Wi-Fi bridge that has forgotten that MAC learns it again. Without
        // this an idle bridge drops the Initial and the dial times out.
        status("Finding the host on the network.");
        let generation = std::sync::atomic::AtomicU64::new(0);
        let _ = etherquic::discover::discover_burst(net.adapter(), &generation, 0);
        let endpoint = l2_client_endpoint(net, connect.local_mac, connect.service_id)?;
        let peer = peer_to_socket_addr(connect.peer_mac, connect.service_id);
        (endpoint, peer, format_mac(connect.peer_mac))
    };
    status(&format!("Dialing {peer_text}."));
    let connecting = match endpoint.connect(peer, "localhost") {
        Ok(connecting) => connecting,
        Err(error) => {
            end_view(&stop, &status, &format!("Connect failed: {error}"));
            shutdown_endpoint(endpoint).await;
            return Ok(());
        }
    };
    let connected = tokio::select! {
        result = tokio::time::timeout(Duration::from_secs(10), connecting) => result,
        _ = until_stop(&stop) => {
            shutdown_endpoint(endpoint).await;
            return Ok(());
        }
    };
    let conn = match connected {
        Ok(Ok(conn)) => conn,
        Ok(Err(error)) => {
            end_view(&stop, &status, &format!("Connect failed: {error}"));
            shutdown_endpoint(endpoint).await;
            return Ok(());
        }
        Err(_) => {
            end_view(&stop, &status, "Connect timed out.");
            shutdown_endpoint(endpoint).await;
            return Ok(());
        }
    };
    let tracing = if connect.trace_input {
        " Tracing input: the host confirms each key and click."
    } else {
        ""
    };
    status(&format!(
        "Viewing {peer_text}. Focus the picture window to use the keyboard and the mouse. A gamepad sends while it is open.{tracing}"
    ));
    let (audio_tx, mut audio_rx) = mpsc::channel(8);
    let (ack_tx, mut ack_rx) = mpsc::channel::<crate::ack::InputAck>(32);
    let acks = if connect.trace_input {
        Some(ack_tx)
    } else {
        None
    };
    let report_status = Arc::clone(&status);
    let report_stamps = Arc::clone(&stamps);
    let report_title = Arc::clone(&title);
    tokio::spawn(async move {
        let mut last_pointer = Instant::now() - Duration::from_secs(1);
        while let Some(ack) = ack_rx.recv().await {
            let elapsed = report_stamps
                .lock()
                .ok()
                .and_then(|mut sent| sent.remove(&ack.time_us))
                .map(|sent| sent.elapsed());
            let line = ack.summary(elapsed);
            let show = ack.kind == crate::ack::ACK_KEY
                || last_pointer.elapsed() >= Duration::from_millis(500);
            if show {
                if ack.kind != crate::ack::ACK_KEY {
                    last_pointer = Instant::now();
                }
                report_status(&line);
                if let Ok(mut title) = report_title.lock() {
                    *title = line;
                }
            }
        }
    });
    let watch = conn.clone();
    let negotiates = etherquic::negotiates(&conn);
    let mut offer = connect.codecs.offer();
    let view_look = connect.view.clone();
    let initial = crate::snapshot(&view_look);
    offer.look = initial.look;
    offer.dither = Some(initial.dither);
    offer.pixel_size = Some(initial.pixel_size);
    offer.dither_strength = Some(initial.dither_strength);
    if connect.codecs.quality > 0 {
        offer.quality = connect.codecs.quality;
    } else {
        offer.quality = initial.quality;
    }
    offer.want_dirty_rects = Some(connect.codecs.dirty_rects || initial.dirty_rects);
    offer.yuv = Some(initial.yuv);
    let (live_tx, live_rx) = tokio::sync::watch::channel(crate::LiveOffer {
        look: offer.look,
        dither: initial.dither,
        pixel_size: initial.pixel_size,
        dither_strength: initial.dither_strength,
        quality: offer.quality,
        dirty_rects: offer.want_dirty_rects.unwrap_or(false),
        yuv: offer.yuv.unwrap_or_default(),
        fps: offer.fps,
    });
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_millis(200)).await;
            let snap = crate::snapshot(&view_look);
            if live_tx
                .send(crate::LiveOffer {
                    look: snap.look,
                    dither: snap.dither,
                    pixel_size: snap.pixel_size,
                    dither_strength: snap.dither_strength,
                    quality: snap.quality,
                    dirty_rects: snap.dirty_rects,
                    yuv: snap.yuv,
                    fps: offer.fps,
                })
                .is_err()
            {
                break;
            }
        }
    });
    if negotiates {
        let video: Vec<&str> = offer.video.iter().map(|row| row.token()).collect();
        let audio: Vec<&str> = offer.audio.iter().map(|row| row.token()).collect();
        eprintln!(
            "etherquic-play: offer video [{}] audio [{}] look {} quality {} dirty-rects {}",
            video.join(" "),
            audio.join(" "),
            offer.look.token(),
            offer.quality,
            if offer.want_dirty_rects == Some(true) {
                1
            } else {
                0
            }
        );
    } else {
        eprintln!("etherquic-play: the host does not negotiate codecs; MJPEG and PCM");
    }
    let shown = frames.clone();
    let sink: etherquic_media::Sink<etherquic_media::Frame> = Arc::new(move |frame| {
        if let Ok(mut slot) = shown.lock() {
            *slot = Some(Picture {
                width: frame.width,
                height: frame.height,
                rgb: frame.rgb,
            });
        }
        true
    });
    let (event_tx, mut event_rx) = mpsc::unbounded_channel();
    let media = crate::PlayerMedia {
        offer,
        frames: sink,
        events: event_tx,
        live: live_rx,
    };
    let mut session =
        tokio::spawn(async move { play_session(conn, media, audio_tx, input, acks, pipes).await });
    let codec_status = Arc::clone(&status);
    let codec_title = Arc::clone(&title);
    let quiet_stats = connect.trace_input;
    let pictures = tokio::spawn(async move {
        let mut line = crate::SessionLine::new(peer_text.clone(), negotiates);
        while let Some(event) = event_rx.recv().await {
            if let etherquic_media::player::PlayerEvent::Cursor(update) = &event {
                cursors
                    .lock()
                    .unwrap_or_else(|poison| poison.into_inner())
                    .apply(update.clone());
                // The hub window redraws from this. The macOS picture loop is
                // woken by `wake_window`, which watches the same sequence.
                picture_hub::wake_picture();
            }
            let show = line.apply(&event);
            if let Some(log) = line.log_line(&event) {
                eprintln!("etherquic-play: {log}");
            }
            if show
                && !(quiet_stats && matches!(event, etherquic_media::player::PlayerEvent::Stats(_)))
            {
                let text = line.text();
                codec_status(&text);
                if let Ok(mut title) = codec_title.lock() {
                    *title = line.short();
                }
            }
        }
    });
    let sound = tokio::spawn(async move {
        let mut logged = false;
        while let Some(chunk) = audio_rx.recv().await {
            if !logged && chunk.sample_rate > 0 && chunk.channels > 0 && !chunk.pcm.is_empty() {
                eprintln!(
                    "etherquic-play: audio {} Hz {} ch pcm {} samples",
                    chunk.sample_rate,
                    chunk.channels,
                    chunk.pcm.len()
                );
                logged = true;
            }
            if speaker.send(chunk).is_err() {
                break;
            }
        }
    });
    // `play` owns one connection handle. This one notices a dead peer even
    // when a stream read is still parked, then the window is told to close.
    let ended = tokio::select! {
        biased;
        _ = until_stop(&stop) => LiveEnd::Stopped,
        result = &mut session => LiveEnd::Finished(result),
        reason = watch.closed() => LiveEnd::Lost(reason.to_string()),
    };
    session.abort();
    pictures.abort();
    sound.abort();
    if !stop.load(Ordering::SeqCst) {
        let line = match ended {
            LiveEnd::Stopped => String::new(),
            LiveEnd::Finished(Ok(Ok(()))) => "The host closed the session.".to_string(),
            LiveEnd::Finished(Ok(Err(error))) => session_end_status(&error.to_string()),
            LiveEnd::Finished(Err(error)) => session_end_status(&error.to_string()),
            LiveEnd::Lost(reason) => session_end_status(&reason),
        };
        // The picture window watches this flag. Without it the last frame
        // stays up and the connect thread never returns.
        end_view(&stop, &status, &line);
    }
    drop(watch);
    shutdown_endpoint(endpoint).await;
    Ok(())
}

/// Tell the window to close, unless the user already pressed Stop.
fn end_view(stop: &AtomicBool, status: &Status, line: &str) {
    if stop.load(Ordering::SeqCst) {
        return;
    }
    if !line.is_empty() {
        status(line);
    }
    stop.store(true, Ordering::SeqCst);
}

/// Send CONNECTION_CLOSE, but do not wait out a peer that will never answer.
async fn shutdown_endpoint(endpoint: quinn::Endpoint) {
    endpoint.close(0u32.into(), b"stop");
    let _ = tokio::time::timeout(Duration::from_millis(400), endpoint.wait_idle()).await;
}

enum LiveEnd {
    Stopped,
    Finished(Result<Result<(), crate::Error>, tokio::task::JoinError>),
    Lost(String),
}

async fn until_stop(stop: &AtomicBool) {
    while !stop.load(Ordering::SeqCst) {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// The output stream stays on this thread. ALSA's handle is not `Send`.
fn play_audio(chunks: std::sync::mpsc::Receiver<AudioChunk>, output: Option<String>) {
    let queue = Arc::new(Mutex::new(VecDeque::<i16>::new()));
    let mut stream = None;
    let mut channels = 0u16;
    let mut rate = 0u32;
    while let Ok(chunk) = chunks.recv() {
        if stream.is_none() {
            match open_output(queue.clone(), output.as_deref()) {
                Ok((opened, opened_channels, opened_rate)) => {
                    channels = opened_channels;
                    rate = opened_rate;
                    stream = Some(opened);
                    eprintln!("etherquic-play: playing {rate} Hz {channels} ch");
                }
                Err(error) => {
                    eprintln!("etherquic-play: playback: {error}");
                    continue;
                }
            }
        }
        let samples = match resample(&chunk, channels, rate) {
            Ok(samples) => samples,
            Err(error) => {
                eprintln!("etherquic-play: playback: {error}");
                continue;
            }
        };
        let mut queued = match queue.lock() {
            Ok(queued) => queued,
            Err(poison) => poison.into_inner(),
        };
        queued.extend(samples);
        let cap = usize::from(channels.max(1)) * usize::try_from(rate).unwrap_or(48_000);
        while queued.len() > cap {
            queued.pop_front();
        }
    }
}

fn open_output(
    queue: Arc<Mutex<VecDeque<i16>>>,
    wanted: Option<&str>,
) -> Result<(cpal::Stream, u16, u32), String> {
    let host = cpal::default_host();
    // A named device that is gone falls back to the default.
    let named = wanted.and_then(|wanted| {
        host.output_devices()
            .ok()?
            .find(|device| device.name().map(|name| name == wanted).unwrap_or(false))
    });
    let device = named
        .or_else(|| host.default_output_device())
        .ok_or("no audio output device")?;
    let config = device
        .default_output_config()
        .map_err(|error| error.to_string())?;
    let channels = config.channels();
    let rate = config.sample_rate().0;
    let stream = match config.sample_format() {
        SampleFormat::F32 => output_stream::<f32>(&device, &config, queue)?,
        SampleFormat::I16 => output_stream::<i16>(&device, &config, queue)?,
        SampleFormat::U16 => output_stream::<u16>(&device, &config, queue)?,
        other => return Err(format!("unsupported output format {other}")),
    };
    stream.play().map_err(|error| error.to_string())?;
    Ok((stream, channels, rate))
}

fn output_stream<T>(
    device: &cpal::Device,
    config: &cpal::SupportedStreamConfig,
    queue: Arc<Mutex<VecDeque<i16>>>,
) -> Result<cpal::Stream, String>
where
    T: SizedSample + FromSample<f32>,
{
    let stream = device
        .build_output_stream(
            &config.config(),
            move |data: &mut [T], _| {
                let mut samples = match queue.try_lock() {
                    Ok(samples) => samples,
                    Err(_) => {
                        data.fill(T::from_sample(0.0));
                        return;
                    }
                };
                for slot in data.iter_mut() {
                    let sample = samples.pop_front().unwrap_or(0);
                    *slot = T::from_sample(sample as f32 / i16::MAX as f32);
                }
            },
            |error| eprintln!("etherquic-play: playback: {error}"),
            None,
        )
        .map_err(|error| error.to_string())?;
    Ok(stream)
}

struct App {
    frames: Arc<Mutex<Option<Picture>>>,
    input: Arc<Gate>,
    stop: Arc<AtomicBool>,
    window: Option<Arc<Window>>,
    context: Option<Context<Arc<Window>>>,
    surface: Option<Surface<Arc<Window>, Arc<Window>>>,
    picture: Option<Picture>,
    /// Previous picture RGB, for the dirty-tile overlay.
    debug_prev: Option<(u32, u32, Vec<u8>)>,
    debug_tiles: Vec<etherquic_media::dirty::Rect>,
    debug_on: bool,
    buttons: u8,
    focused: bool,
    trace: bool,
    clock: TraceClock,
    opened: Instant,
    stamps: Arc<Mutex<HashMap<u64, Instant>>>,
    title: Arc<Mutex<String>>,
    shortcuts: Option<ShortcutHold>,
    modifiers: ModifiersState,
    /// Local pointer, in window pixels. This is what we send to the host.
    cursor: (f64, f64),
    remote_cursor: Arc<Mutex<etherquic::cursor::CursorView>>,
    cursor_mode: crate::CursorMode,
    cursor_seq: u64,
    cursor_visible: bool,
    cursor_icon: Option<CursorIcon>,
    /// `image_id` of the custom cursor installed for Copy mode. `0` means none.
    cursor_image: u32,
    letterbox: crate::Placement,
    view: crate::LiveView,
    /// Picture size the window was last fitted to, for "window matches host".
    fitted: (u32, u32),
    /// When set, this window draws whichever seat [`picture_hub`] has shown.
    hub: bool,
    bound_id: u64,
    /// Idle until the HWND exists, Showing while it is on screen, Closing
    /// after close until Windows has destroyed it, then Closed.
    phase: Phase,
}

/// Picture-window lifecycle. Close moves to Closing and keeps the event
/// loop pumping until the HWND is destroyed. Exiting in the same turn as
/// dropping the window posts a destroy message that nobody reads, and
/// Windows then marks the window Not Responding.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    Idle,
    Showing,
    Closing,
    Closed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CloseAction {
    /// A close is already in progress.
    Ignore,
    /// Another picture is still open. Keep this HWND and draw that one.
    Rebind,
    /// Nothing left to show. Drop the window and wait for it to be destroyed.
    Destroy,
}

impl Phase {
    fn action(self, other_seat: bool) -> CloseAction {
        match self {
            Phase::Closing | Phase::Closed => CloseAction::Ignore,
            Phase::Idle | Phase::Showing if other_seat => CloseAction::Rebind,
            Phase::Idle | Phase::Showing => CloseAction::Destroy,
        }
    }
}

/// Blocks compositor shortcuts while the picture window is focused, so the
/// Windows key reaches this window. Dropped when focus leaves or the view closes.
struct ShortcutHold;

impl ShortcutHold {
    fn grab() -> Self {
        block_shortcuts(true);
        Self
    }
}

impl Drop for ShortcutHold {
    fn drop(&mut self) {
        block_shortcuts(false);
    }
}

fn block_shortcuts(block: bool) {
    #[cfg(target_os = "linux")]
    {
        let flag = if block { "true" } else { "false" };
        // This call can sit on the bus. It used to run on the window thread,
        // which meant a close click was not handled until busctl returned.
        thread::spawn(move || {
            let _ = std::process::Command::new("busctl")
                .args([
                    "--user",
                    "call",
                    "org.kde.kglobalaccel",
                    "/kglobalaccel",
                    "org.kde.KGlobalAccel",
                    "blockGlobalShortcuts",
                    "b",
                    flag,
                ])
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status();
        });
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = block;
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if matches!(self.phase, Phase::Closing | Phase::Closed) {
            return;
        }
        if self.hub {
            self.follow_board(event_loop);
            if matches!(self.phase, Phase::Closing | Phase::Closed) || self.bound_id == 0 {
                return;
            }
        } else if self.stop.load(Ordering::SeqCst) {
            self.begin_close(event_loop);
            return;
        }
        if self.window.is_some() {
            return;
        }
        self.open_window(event_loop);
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => self.begin_close(event_loop),
            WindowEvent::Destroyed => self.note_destroyed(event_loop),
            WindowEvent::Focused(focused) => {
                if !self.live() {
                    return;
                }
                self.focused = focused;
                if focused {
                    if self.shortcuts.is_none() {
                        self.shortcuts = Some(ShortcutHold::grab());
                    }
                } else {
                    self.shortcuts.take();
                }
            }
            WindowEvent::RedrawRequested => self.draw(event_loop),
            WindowEvent::Resized(_) => self.draw(event_loop),
            WindowEvent::ModifiersChanged(modifiers) => {
                if !self.live() {
                    return;
                }
                self.send_modifiers(modifiers.state());
            }
            WindowEvent::KeyboardInput { event, .. } => {
                if !self.live() {
                    return;
                }
                if self.focused
                    && crate::text_paste_watched()
                    && crate::paste_chord(
                        cfg!(target_os = "macos"),
                        self.modifiers.control_key(),
                        self.modifiers.alt_key(),
                        self.modifiers.super_key(),
                        key_is_v(&event.logical_key),
                    )
                {
                    if event.state == ElementState::Pressed && !event.repeat {
                        crate::request_text_paste();
                    }
                    return;
                }
                if !crate::snapshot(&self.view).send_keyboard || !self.focused {
                    return;
                }
                // Auto-repeat of letters floods the host. Arrows, Home, and
                // the other dedicated nav keys must keep repeating so a hold
                // still moves.
                if event.repeat && event.state == ElementState::Pressed {
                    let nav = physical_nav_keysym(event.physical_key).is_some()
                        || matches!(
                            event.logical_key,
                            Key::Named(
                                NamedKey::ArrowLeft
                                    | NamedKey::ArrowRight
                                    | NamedKey::ArrowUp
                                    | NamedKey::ArrowDown
                                    | NamedKey::Home
                                    | NamedKey::End
                                    | NamedKey::PageUp
                                    | NamedKey::PageDown
                                    | NamedKey::Insert
                                    | NamedKey::Delete
                            )
                        );
                    if !nav {
                        return;
                    }
                }
                self.send_key(
                    &event.logical_key,
                    event.physical_key,
                    event.state == ElementState::Pressed,
                );
            }
            WindowEvent::CursorMoved { position, .. } => {
                if !self.live() {
                    return;
                }
                self.cursor = (position.x, position.y);
                self.send_pointer(0, 0);
            }
            WindowEvent::MouseInput { state, button, .. } => {
                if !self.live() {
                    return;
                }
                let bit = match button {
                    MouseButton::Left => Buttons::LEFT,
                    MouseButton::Right => Buttons::RIGHT,
                    MouseButton::Middle => Buttons::MIDDLE,
                    MouseButton::Other(_) | MouseButton::Back | MouseButton::Forward => 0,
                };
                if state == ElementState::Pressed {
                    self.buttons |= bit;
                } else {
                    self.buttons &= !bit;
                }
                self.send_pointer(0, 0);
            }
            WindowEvent::MouseWheel { delta, .. } => {
                if !self.live() {
                    return;
                }
                let (x, y) = match delta {
                    MouseScrollDelta::LineDelta(x, y) => (x.round() as i16, y.round() as i16),
                    MouseScrollDelta::PixelDelta(px) => {
                        ((px.x / 40.0).round() as i16, (px.y / 40.0).round() as i16)
                    }
                };
                self.send_pointer(x, y);
            }
            _ => {}
        }
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, _event: ()) {
        self.follow_board(event_loop);
        self.take_frame();
        self.apply_title();
        self.drain_keystroke_strings();
        self.sync_cursor(event_loop);
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        self.follow_board(event_loop);
        self.take_frame();
        self.apply_title();
        self.drain_keystroke_strings();
        self.sync_cursor(event_loop);
    }

    fn exiting(&mut self, _event_loop: &ActiveEventLoop) {
        // Drop the window before winit flushes this run, so the next connect
        // can create another one on the same loop.
        self.drop_window();
    }
}

fn player_icon() -> Option<Icon> {
    let bytes = include_bytes!(concat!(env!("OUT_DIR"), "/player-256.rgba"));
    if bytes.len() < 8 {
        return None;
    }
    let width = u32::from_le_bytes(bytes[0..4].try_into().ok()?);
    let height = u32::from_le_bytes(bytes[4..8].try_into().ok()?);
    if width == 0 || height == 0 {
        return None;
    }
    let rgba = bytes[8..].to_vec();
    if rgba.len() != (width as usize) * (height as usize) * 4 {
        return None;
    }
    Icon::from_rgba(rgba, width, height).ok()
}

impl App {
    fn for_hub() -> Self {
        Self {
            frames: Arc::new(Mutex::new(None)),
            input: Arc::new(Gate::new()),
            stop: Arc::new(AtomicBool::new(false)),
            window: None,
            context: None,
            surface: None,
            picture: None,
            debug_prev: None,
            debug_tiles: Vec::new(),
            debug_on: false,
            buttons: 0,
            focused: false,
            trace: false,
            clock: TraceClock::default(),
            opened: Instant::now(),
            stamps: Arc::new(Mutex::new(HashMap::new())),
            title: Arc::new(Mutex::new(String::new())),
            shortcuts: None,
            modifiers: ModifiersState::empty(),
            cursor: (0.0, 0.0),
            remote_cursor: etherquic::cursor::share(),
            cursor_mode: crate::CursorMode::Native,
            cursor_seq: 0,
            cursor_visible: true,
            cursor_icon: None,
            cursor_image: 0,
            letterbox: crate::place(crate::ScaleMode::Fit, 0, 0, 1, 1),
            view: crate::live(crate::ViewOptions::default()),
            fitted: (0, 0),
            hub: true,
            bound_id: 0,
            phase: Phase::Idle,
        }
    }

    fn apply_seat(&mut self, seat: PictureSeat) {
        self.bound_id = seat.id;
        self.frames = seat.frames;
        self.input = seat.input;
        self.stop = seat.stop;
        self.trace = seat.trace;
        self.view = seat.view;
        self.stamps = seat.stamps;
        self.title = seat.title;
        self.remote_cursor = Arc::clone(&seat.cursor);
        // A value the next sample cannot match, so the new seat's cursor is applied.
        self.cursor_seq = u64::MAX;
        self.cursor_icon = None;
        self.cursor_image = 0;
        self.picture = None;
        self.debug_prev = None;
        self.debug_tiles.clear();
        self.debug_on = false;
        self.buttons = 0;
    }

    fn bind_shown(&mut self) -> bool {
        let Some(seat) = picture_hub::screen_seat() else {
            return false;
        };
        self.apply_seat(seat);
        true
    }

    /// Rebind when the shown picture changes. Closing the bound seat does not
    /// wait for any other session, and it does not exit while an HWND is
    /// still on screen.
    fn follow_board(&mut self, event_loop: &ActiveEventLoop) {
        if matches!(self.phase, Phase::Closing | Phase::Closed) {
            return;
        }
        if !self.hub {
            if self.stop.load(Ordering::SeqCst) {
                self.begin_close(event_loop);
            }
            return;
        }
        if self.bound_id != 0 && self.stop.load(Ordering::SeqCst) {
            self.begin_close(event_loop);
            return;
        }
        match picture_hub::screen_seat() {
            None if self.window.is_some() => self.begin_close(event_loop),
            None => event_loop.exit(),
            Some(seat) if seat.id != self.bound_id => {
                self.apply_seat(seat);
                self.phase = Phase::Showing;
                self.sync_cursor(event_loop);
                if let Some(window) = &self.window {
                    window.request_redraw();
                }
            }
            Some(_) => {}
        }
    }

    fn live(&self) -> bool {
        self.phase == Phase::Showing
    }

    fn open_window(&mut self, event_loop: &ActiveEventLoop) {
        let window = Arc::new(
            event_loop
                .create_window(
                    Window::default_attributes()
                        .with_title("etherquic-play")
                        .with_active(true)
                        .with_inner_size(PhysicalSize::new(1280, 720))
                        .with_window_icon(player_icon()),
                )
                .expect("window"),
        );
        window.focus_window();
        let context = Context::new(window.clone()).expect("softbuffer context");
        let surface = Surface::new(&context, window.clone()).expect("softbuffer surface");
        self.window = Some(window);
        self.context = Some(context);
        self.surface = Some(surface);
        self.phase = Phase::Showing;
        self.sync_cursor(event_loop);
        // Paint before returning. Until the first present, Windows leaves the
        // client area on the default grey background.
        self.draw(event_loop);
    }

    /// Close the picture on screen. Another open session keeps this window.
    /// The last one drops the HWND and waits until Windows reports it gone.
    fn begin_close(&mut self, event_loop: &ActiveEventLoop) {
        if self.phase.action(false) == CloseAction::Ignore {
            return;
        }
        let other = if self.hub && self.bound_id != 0 {
            let id = self.bound_id;
            self.bound_id = 0;
            let _ = picture_hub::close_picture_quietly(id);
            picture_hub::screen_seat()
        } else {
            if !self.hub {
                self.stop.store(true, Ordering::SeqCst);
            }
            None
        };
        match self.phase.action(other.is_some()) {
            CloseAction::Ignore => {}
            CloseAction::Rebind => {
                if let Some(seat) = other {
                    self.apply_seat(seat);
                    self.picture = None;
                    self.phase = Phase::Showing;
                    self.sync_cursor(event_loop);
                    if let Some(window) = &self.window {
                        window.request_redraw();
                    }
                }
            }
            CloseAction::Destroy => {
                // Release the DC on this thread, then drop the window. Drop
                // only posts a destroy message. The loop has to keep running
                // until that message is handled.
                self.phase = Phase::Closing;
                self.surface.take();
                self.context.take();
                self.shortcuts.take();
                if self.window.take().is_none() {
                    self.phase = Phase::Closed;
                    event_loop.exit();
                }
            }
        }
    }

    fn note_destroyed(&mut self, event_loop: &ActiveEventLoop) {
        self.surface.take();
        self.context.take();
        self.window.take();
        self.shortcuts.take();
        if self.hub && self.bind_shown() {
            self.phase = Phase::Idle;
            self.open_window(event_loop);
            return;
        }
        self.phase = Phase::Closed;
        event_loop.exit();
    }

    fn drop_window(&mut self) {
        // The surface has to go first. Shortcut release talks to the session
        // bus and must not run while the compositor is still waiting for close.
        self.surface.take();
        self.context.take();
        self.window.take();
        self.shortcuts.take();
    }

    fn draw(&mut self, event_loop: &ActiveEventLoop) {
        if !self.live() {
            return;
        }
        self.take_frame();
        let Some(window) = self.window.clone() else {
            return;
        };
        let Some(surface) = self.surface.as_mut() else {
            return;
        };
        let size = window.inner_size();
        let (Some(width), Some(height)) =
            (NonZeroU32::new(size.width), NonZeroU32::new(size.height))
        else {
            return;
        };
        surface.resize(width, height).ok();
        let mut buffer = match surface.buffer_mut() {
            Ok(buffer) => buffer,
            Err(_) => {
                // The last frame is still on screen. Try again shortly, and
                // keep pumping events so close is not stuck behind this draw.
                event_loop.set_control_flow(ControlFlow::WaitUntil(
                    Instant::now() + Duration::from_millis(8),
                ));
                window.request_redraw();
                return;
            }
        };
        event_loop.set_control_flow(ControlFlow::Wait);
        buffer.fill(0);
        if let Some(picture) = &self.picture {
            let scale = crate::snapshot(&self.view).scale;
            let place = crate::place(
                scale,
                picture.width,
                picture.height,
                size.width,
                size.height,
            );
            self.letterbox = place;
            let mut view = vec![0u32; (place.view_w as usize) * (place.view_h as usize)];
            blit_rgb(picture, &mut view, place.view_w, place.view_h);
            if crate::snapshot(&self.view).debug_dirty_rects {
                let color = 0x00FF40;
                let src_w = picture.width.max(1);
                let src_h = picture.height.max(1);
                for rect in &self.debug_tiles {
                    let x0 = (u64::from(rect.x) * u64::from(place.view_w) / u64::from(src_w)) as u32;
                    let y0 = (u64::from(rect.y) * u64::from(place.view_h) / u64::from(src_h)) as u32;
                    let x1 = (u64::from(rect.x.saturating_add(rect.w)) * u64::from(place.view_w)
                        / u64::from(src_w)) as u32;
                    let y1 = (u64::from(rect.y.saturating_add(rect.h)) * u64::from(place.view_h)
                        / u64::from(src_h)) as u32;
                    let w = x1.saturating_sub(x0).max(1);
                    let h = y1.saturating_sub(y0).max(1);
                    stroke_rgb(&mut view, place.view_w, place.view_h, x0, y0, w, h, color);
                }
            }
            for y in 0..place.h {
                let row = ((place.crop_y + y) * place.view_w + place.crop_x) as usize;
                for x in 0..place.w {
                    let dst = ((place.dst_y + y) * size.width + place.dst_x + x) as usize;
                    if dst < buffer.len() {
                        buffer[dst] = view[row + x as usize];
                    }
                }
            }
            if crate::snapshot(&self.view).cursor == crate::CursorMode::Remote {
                let paint = {
                    let drawn = self
                        .remote_cursor
                        .lock()
                        .unwrap_or_else(|poison| poison.into_inner());
                    if !drawn.visible {
                        None
                    } else if drawn.width > 0 && !drawn.bgra.is_empty() {
                        Some((
                            drawn.bgra.clone(),
                            u32::from(drawn.width),
                            u32::from(drawn.height),
                            i32::from(drawn.hotspot_x),
                            i32::from(drawn.hotspot_y),
                            drawn.x,
                            drawn.y,
                        ))
                    } else {
                        let stand_in = etherquic::cursor::fallback(drawn.shape);
                        Some((
                            stand_in.bgra,
                            u32::from(stand_in.width),
                            u32::from(stand_in.height),
                            i32::from(stand_in.hotspot_x),
                            i32::from(stand_in.hotspot_y),
                            drawn.x,
                            drawn.y,
                        ))
                    }
                };
                if let Some((bgra, bw, bh, hx, hy, x, y)) = paint {
                    let place = etherquic::cursor::Placed {
                        src_w: picture.width,
                        src_h: picture.height,
                        view_w: self.letterbox.view_w,
                        view_h: self.letterbox.view_h,
                        crop_x: self.letterbox.crop_x,
                        crop_y: self.letterbox.crop_y,
                        dst_x: self.letterbox.dst_x,
                        dst_y: self.letterbox.dst_y,
                    };
                    let sprite = etherquic::cursor::sprite_box(place, x, y, hx, hy, bw, bh);
                    etherquic::cursor::paint_sprite(
                        &mut buffer,
                        size.width,
                        size.height,
                        sprite,
                        &bgra,
                        bw,
                        bh,
                    );
                }
            }
        }
        let _ = buffer.present();
    }

    /// Native and Copy keep the local cursor and change its shape from the
    /// host. Remote hides it; the picture pass draws the host cursor instead.
    fn sync_cursor(&mut self, event_loop: &ActiveEventLoop) {
        if !self.live() {
            return;
        }
        let Some(window) = self.window.clone() else {
            return;
        };
        let mode = crate::snapshot(&self.view).cursor;
        let (seq, shape, image_id, width, height, hotspot_x, hotspot_y, bgra) = {
            let guard = self
                .remote_cursor
                .lock()
                .unwrap_or_else(|poison| poison.into_inner());
            if mode == self.cursor_mode && guard.seq == self.cursor_seq {
                return;
            }
            let need_bitmap = mode == crate::CursorMode::Copy
                && guard.width > 0
                && !guard.bgra.is_empty()
                && (guard.image_id != self.cursor_image
                    || self.cursor_mode != crate::CursorMode::Copy);
            let bgra = if need_bitmap {
                guard.bgra.clone()
            } else {
                Vec::new()
            };
            (
                guard.seq,
                guard.shape,
                guard.image_id,
                guard.width,
                guard.height,
                guard.hotspot_x,
                guard.hotspot_y,
                bgra,
            )
        };
        let was_remote = self.cursor_mode == crate::CursorMode::Remote;
        match mode {
            crate::CursorMode::Native => {
                if !self.cursor_visible {
                    window.set_cursor_visible(true);
                }
                let icon = shape_icon(shape);
                if self.cursor_icon != Some(icon) || self.cursor_image != 0 || !self.cursor_visible
                {
                    window.set_cursor(icon);
                    self.cursor_image = 0;
                }
                self.cursor_icon = Some(icon);
                self.cursor_visible = true;
            }
            crate::CursorMode::Copy => {
                if !self.cursor_visible {
                    window.set_cursor_visible(true);
                }
                if !bgra.is_empty()
                    && install_cursor(
                        event_loop, &window, &bgra, width, height, hotspot_x, hotspot_y,
                    )
                {
                    self.cursor_image = image_id;
                    self.cursor_icon = None;
                } else {
                    let same_bitmap = self.cursor_mode == crate::CursorMode::Copy
                        && self.cursor_image != 0
                        && self.cursor_image == image_id;
                    if !same_bitmap {
                        let icon = shape_icon(shape);
                        if self.cursor_icon != Some(icon) || self.cursor_image != 0 {
                            window.set_cursor(icon);
                        }
                        self.cursor_icon = Some(icon);
                        self.cursor_image = 0;
                    }
                }
                self.cursor_visible = true;
            }
            crate::CursorMode::Remote => {
                if self.cursor_visible {
                    window.set_cursor_visible(false);
                    self.cursor_visible = false;
                }
            }
        }
        if mode == crate::CursorMode::Remote || was_remote {
            window.request_redraw();
        }
        self.cursor_mode = mode;
        self.cursor_seq = seq;
    }

    fn take_frame(&mut self) {
        let options = crate::snapshot(&self.view);
        if options.debug_dirty_rects != self.debug_on {
            self.debug_on = options.debug_dirty_rects;
            if self.debug_on {
                if let Some(picture) = &self.picture {
                    self.debug_prev = Some((picture.width, picture.height, picture.rgb.clone()));
                }
            } else {
                self.debug_prev = None;
                self.debug_tiles.clear();
            }
            if let Some(window) = &self.window {
                window.request_redraw();
            }
        }
        let next = self.frames.lock().ok().and_then(|mut slot| slot.take());
        if let Some(picture) = next {
            if self.debug_on {
                self.debug_tiles = match &self.debug_prev {
                    Some((w, h, prev)) if *w == picture.width && *h == picture.height => {
                        etherquic_media::dirty::changed_tiles(
                            prev,
                            &picture.rgb,
                            picture.width,
                            picture.height,
                        )
                    }
                    _ => Vec::new(),
                };
                self.debug_prev = Some((picture.width, picture.height, picture.rgb.clone()));
            }
            let size = (picture.width, picture.height);
            self.picture = Some(picture);
            if let Some(window) = &self.window {
                if options.size_match == crate::SizeMatch::WindowToHost && size != self.fitted {
                    self.fitted = size;
                    let _ = window.request_inner_size(PhysicalSize::new(size.0, size.1));
                }
                window.request_redraw();
            }
        } else if let Some(window) = &self.window {
            // A scale change made in the console shows without a new frame.
            if let Some(picture) = &self.picture {
                let size = window.inner_size();
                let place = crate::place(
                    options.scale,
                    picture.width,
                    picture.height,
                    size.width,
                    size.height,
                );
                if place != self.letterbox {
                    window.request_redraw();
                }
            }
        }
    }

    fn send_pointer(&mut self, wheel_x: i16, wheel_y: i16) {
        if self.picture.is_none() {
            return;
        }
        if !crate::snapshot(&self.view).send_mouse {
            return;
        }
        let (x, y) = self.cursor;
        let (px, py) = crate::to_remote(&self.letterbox, x, y);
        self.emit(vec![Event {
            time_us: 0,
            body: Body::Pointer {
                x: px,
                y: py,
                buttons: self.buttons,
                wheel_y,
                wheel_x,
            },
        }]);
    }

    fn apply_title(&self) {
        let next = self.title.lock().ok().and_then(|mut title| {
            if title.is_empty() {
                None
            } else {
                Some(std::mem::take(&mut *title))
            }
        });
        if let Some(title) = next {
            if let Some(window) = &self.window {
                window.set_title(&title);
            }
        }
    }

    /// A typed string. This does not look at focus, so it still sends when the
    /// picture window is in the background. The key handler above does not.
    ///
    /// Held modifiers are released for the string and pressed again after it,
    /// so a Ctrl+V paste types the characters rather than chords.
    fn drain_keystroke_strings(&mut self) {
        let pending = PENDING_KEYSTROKES
            .lock()
            .map(|mut queue| std::mem::take(&mut *queue))
            .unwrap_or_default();
        if pending.is_empty() {
            return;
        }
        let lift = crate::snapshot(&self.view).send_keyboard && self.modifiers_held();
        if lift {
            self.chord_break(false);
        }
        for text in pending {
            let events = crate::keystroke_events(&text);
            if !events.is_empty() {
                self.emit(events);
            }
        }
        if lift {
            self.chord_break(true);
        }
    }

    fn modifiers_held(&self) -> bool {
        self.modifiers.shift_key()
            || self.modifiers.control_key()
            || self.modifiers.alt_key()
            || self.modifiers.super_key()
    }

    /// `restore` presses the held modifiers again. Otherwise it releases them.
    /// `self.modifiers` stays the physical state.
    fn chord_break(&mut self, restore: bool) {
        let held = [
            (self.modifiers.shift_key(), 0xffe1),
            (self.modifiers.control_key(), 0xffe3),
            (self.modifiers.alt_key(), 0xffe9),
            (self.modifiers.super_key(), 0xffeb),
        ];
        let events = held
            .into_iter()
            .filter(|(down, _)| *down)
            .map(|(_, code)| Event {
                time_us: 0,
                body: Body::Key {
                    down: restore,
                    source: KeySource::Keysym,
                    code,
                },
            })
            .collect::<Vec<_>>();
        if !events.is_empty() {
            self.emit(events);
        }
    }

    fn send_key(&mut self, key: &Key, physical: PhysicalKey, down: bool) {
        if !crate::snapshot(&self.view).send_keyboard {
            return;
        }
        let Some(code) = keysym(key, physical) else {
            return;
        };
        self.emit(vec![Event {
            time_us: 0,
            body: Body::Key {
                down,
                source: KeySource::Keysym,
                code,
            },
        }]);
    }

    fn send_modifiers(&mut self, next: ModifiersState) {
        if !crate::snapshot(&self.view).send_keyboard {
            self.modifiers = next;
            return;
        }
        let mut events = Vec::new();
        for (was, now, code) in [
            (self.modifiers.shift_key(), next.shift_key(), 0xffe1),
            (self.modifiers.control_key(), next.control_key(), 0xffe3),
            (self.modifiers.alt_key(), next.alt_key(), 0xffe9),
            (self.modifiers.super_key(), next.super_key(), 0xffeb),
        ] {
            if was != now {
                events.push(Event {
                    time_us: 0,
                    body: Body::Key {
                        down: now,
                        source: KeySource::Keysym,
                        code,
                    },
                });
            }
        }
        self.modifiers = next;
        if !events.is_empty() {
            self.emit(events);
        }
    }

    fn emit(&mut self, mut events: Vec<Event>) {
        let now = self.opened.elapsed();
        for event in &mut events {
            if self.trace && self.clock.stamp(now, event) && event.time_us != 0 {
                if let Ok(mut sent) = self.stamps.lock() {
                    sent.insert(event.time_us, Instant::now());
                    if sent.len() > 256 {
                        sent.clear();
                        sent.insert(event.time_us, Instant::now());
                    }
                }
            } else if !self.trace {
                event.time_us = 0;
            }
            self.input.push(event.clone());
        }
    }
}

fn shape_icon(shape: etherquic::cursor::CursorShape) -> CursorIcon {
    use etherquic::cursor::CursorShape;
    match shape {
        CursorShape::Arrow | CursorShape::Custom => CursorIcon::Default,
        CursorShape::Hand => CursorIcon::Pointer,
        CursorShape::IBeam => CursorIcon::Text,
        CursorShape::Wait => CursorIcon::Wait,
        CursorShape::Cross => CursorIcon::Crosshair,
        CursorShape::SizeNs => CursorIcon::NsResize,
        CursorShape::SizeWe => CursorIcon::EwResize,
        CursorShape::SizeNwse => CursorIcon::NwseResize,
        CursorShape::SizeNesw => CursorIcon::NeswResize,
        CursorShape::SizeAll => CursorIcon::Move,
        CursorShape::No => CursorIcon::NotAllowed,
        CursorShape::Help => CursorIcon::Help,
        CursorShape::UpArrow => CursorIcon::NResize,
        CursorShape::AppStarting => CursorIcon::Progress,
    }
}

fn clamp_hotspot(hot: i16, edge: u16) -> u16 {
    let last = edge.saturating_sub(1);
    if hot <= 0 {
        0
    } else {
        u16::try_from(hot).unwrap_or(u16::MAX).min(last)
    }
}

fn install_cursor(
    event_loop: &ActiveEventLoop,
    window: &Window,
    bgra: &[u8],
    width: u16,
    height: u16,
    hotspot_x: i16,
    hotspot_y: i16,
) -> bool {
    if width == 0 || height == 0 {
        return false;
    }
    let rgba = etherquic::cursor::bgra_to_rgba(bgra);
    let Ok(source) = CustomCursor::from_rgba(
        rgba,
        width,
        height,
        clamp_hotspot(hotspot_x, width),
        clamp_hotspot(hotspot_y, height),
    ) else {
        return false;
    };
    window.set_cursor(event_loop.create_custom_cursor(source));
    true
}

fn key_is_v(key: &Key) -> bool {
    match key {
        Key::Character(text) => text.eq_ignore_ascii_case("v"),
        _ => false,
    }
}

fn keysym(key: &Key, physical: PhysicalKey) -> Option<u32> {
    // Dedicated cluster (ArrowLeft, Home, …) stays that cluster even when
    // NumLock makes the OS report a digit. The numpad is a different physical
    // key: NumLock on sends KP_4, NumLock off sends KP_Left.
    if let Some(code) = dedicated_nav_keysym(physical) {
        return Some(code);
    }
    if let Some(code) = keypad_keysym(physical, key) {
        return Some(code);
    }
    match key {
        Key::Character(text) => {
            let mut chars = text.chars();
            let ch = chars.next()?;
            if chars.next().is_some() || !ch.is_ascii() {
                return Some(u32::from(ch));
            }
            Some(u32::from(ch))
        }
        Key::Named(named) => named_keysym(*named),
        Key::Unidentified(_) | Key::Dead(_) => None,
    }
}

fn dedicated_nav_keysym(physical: PhysicalKey) -> Option<u32> {
    let PhysicalKey::Code(code) = physical else {
        return None;
    };
    Some(match code {
        KeyCode::ArrowLeft => 0xff51,
        KeyCode::ArrowUp => 0xff52,
        KeyCode::ArrowRight => 0xff53,
        KeyCode::ArrowDown => 0xff54,
        KeyCode::Home => 0xff50,
        KeyCode::End => 0xff57,
        KeyCode::PageUp => 0xff55,
        KeyCode::PageDown => 0xff56,
        KeyCode::Insert => 0xff63,
        KeyCode::Delete => 0xffff,
        _ => return None,
    })
}

fn physical_nav_keysym(physical: PhysicalKey) -> Option<u32> {
    dedicated_nav_keysym(physical)
}

/// Numpad physical keys. NumLock on is a KP digit. NumLock off is KP navigation.
fn keypad_keysym(physical: PhysicalKey, key: &Key) -> Option<u32> {
    let PhysicalKey::Code(code) = physical else {
        return None;
    };
    let (nav, digit) = match code {
        KeyCode::Numpad0 => (0xff9e, 0xffb0),
        KeyCode::Numpad1 => (0xff9c, 0xffb1),
        KeyCode::Numpad2 => (0xff99, 0xffb2),
        KeyCode::Numpad3 => (0xff9b, 0xffb3),
        KeyCode::Numpad4 => (0xff96, 0xffb4),
        KeyCode::Numpad5 => (0xff9d, 0xffb5),
        KeyCode::Numpad6 => (0xff98, 0xffb6),
        KeyCode::Numpad7 => (0xff95, 0xffb7),
        KeyCode::Numpad8 => (0xff97, 0xffb8),
        KeyCode::Numpad9 => (0xff9a, 0xffb9),
        KeyCode::NumpadDecimal => (0xff9f, 0xffae),
        KeyCode::NumpadEnter => return Some(0xff8d),
        KeyCode::NumpadAdd => return Some(0xffab),
        KeyCode::NumpadSubtract => return Some(0xffad),
        KeyCode::NumpadMultiply => return Some(0xffaa),
        KeyCode::NumpadDivide => return Some(0xffaf),
        _ => return None,
    };
    match key {
        Key::Character(_) => Some(digit),
        Key::Named(_) | Key::Unidentified(_) => Some(nav),
        Key::Dead(_) => None,
    }
}

fn named_keysym(key: NamedKey) -> Option<u32> {
    Some(match key {
        NamedKey::Escape => 0xff1b,
        NamedKey::Space => 0x20,
        NamedKey::Enter => 0xff0d,
        NamedKey::Backspace => 0xff08,
        NamedKey::Tab => 0xff09,
        NamedKey::Delete => 0xffff,
        NamedKey::Insert => 0xff63,
        NamedKey::ArrowLeft => 0xff51,
        NamedKey::ArrowUp => 0xff52,
        NamedKey::ArrowRight => 0xff53,
        NamedKey::ArrowDown => 0xff54,
        NamedKey::Home => 0xff50,
        NamedKey::End => 0xff57,
        NamedKey::PageUp => 0xff55,
        NamedKey::PageDown => 0xff56,
        NamedKey::Shift => 0xffe1,
        NamedKey::Control => 0xffe3,
        NamedKey::Alt => 0xffe9,
        NamedKey::Super => 0xffeb,
        NamedKey::F1 => 0xffbe,
        NamedKey::F2 => 0xffbf,
        NamedKey::F3 => 0xffc0,
        NamedKey::F4 => 0xffc1,
        NamedKey::F5 => 0xffc2,
        NamedKey::F6 => 0xffc3,
        NamedKey::F7 => 0xffc4,
        NamedKey::F8 => 0xffc5,
        NamedKey::F9 => 0xffc6,
        NamedKey::F10 => 0xffc7,
        NamedKey::F11 => 0xffc8,
        NamedKey::F12 => 0xffc9,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::{keysym, named_keysym, physical_nav_keysym, CloseAction, Phase};
    use winit::keyboard::{Key, KeyCode, NamedKey, PhysicalKey};

    #[test]
    fn close_keeps_pumping_until_the_window_is_destroyed() {
        assert_eq!(Phase::Showing.action(true), CloseAction::Rebind);
        assert_eq!(Phase::Showing.action(false), CloseAction::Destroy);
        assert_eq!(Phase::Idle.action(false), CloseAction::Destroy);
        assert_eq!(Phase::Closing.action(false), CloseAction::Ignore);
        assert_eq!(Phase::Closing.action(true), CloseAction::Ignore);
        assert_eq!(Phase::Closed.action(true), CloseAction::Ignore);
    }

    #[test]
    fn dedicated_arrows_stay_arrows_when_the_os_reports_a_digit() {
        assert_eq!(named_keysym(NamedKey::ArrowLeft), Some(0xff51));
        assert_eq!(named_keysym(NamedKey::ArrowDown), Some(0xff54));
        let left = PhysicalKey::Code(KeyCode::ArrowLeft);
        assert_eq!(physical_nav_keysym(left), Some(0xff51));
        assert_eq!(keysym(&Key::Character("4".into()), left), Some(0xff51));
        assert_eq!(
            keysym(
                &Key::Character("2".into()),
                PhysicalKey::Code(KeyCode::ArrowDown)
            ),
            Some(0xff54)
        );
        let pad4 = PhysicalKey::Code(KeyCode::Numpad4);
        assert_eq!(physical_nav_keysym(pad4), None);
        // Numpad 4, NumLock on: keypad 4, not the top-row 4 and not Left.
        assert_eq!(keysym(&Key::Character("4".into()), pad4), Some(0xffb4));
        // Numpad 4, NumLock off: KP_Left, not the dedicated Left arrow.
        assert_eq!(keysym(&Key::Named(NamedKey::ArrowLeft), pad4), Some(0xff96));
        assert_eq!(
            keysym(
                &Key::Named(NamedKey::ArrowUp),
                PhysicalKey::Code(KeyCode::Numpad8)
            ),
            Some(0xff97)
        );
        assert_eq!(
            keysym(
                &Key::Character("8".into()),
                PhysicalKey::Code(KeyCode::Numpad8)
            ),
            Some(0xffb8)
        );
    }
}
