//! Player window. Focus it to send the keyboard and the mouse.
//! Gamepads are read on their own thread for as long as the view is open.

use std::collections::{HashMap, VecDeque};
use std::net::SocketAddr;
use std::num::NonZeroU32;
use std::sync::atomic::{AtomicBool, Ordering};
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
use winit::keyboard::{Key, ModifiersState, NamedKey};
use winit::platform::run_on_demand::EventLoopExtRunOnDemand;
use winit::window::{Window, WindowId};

use crate::ack::TraceClock;
use crate::outbox::Gate;
use crate::{
    blit_rgb, format_mac, play, resample, session_end_status, AudioChunk, Picture,
    Status,
};

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
    proxy: std::sync::mpsc::Sender<Result<EventLoopProxy<()>, String>>,
    done: std::sync::mpsc::Sender<Result<(), String>>,
}

#[cfg_attr(target_os = "macos", allow(dead_code))]
fn window_sender() -> Result<std::sync::mpsc::Sender<ViewRequest>, String> {
    static SLOT: OnceLock<std::sync::mpsc::Sender<ViewRequest>> = OnceLock::new();
    static INIT: Mutex<()> = Mutex::new(());
    if let Some(sender) = SLOT.get() {
        return Ok(sender.clone());
    }
    let _guard = INIT.lock().unwrap_or_else(|poison| poison.into_inner());
    if let Some(sender) = SLOT.get() {
        return Ok(sender.clone());
    }
    let (sender, receiver) = std::sync::mpsc::channel();
    thread::Builder::new()
        .name("etherquic-play-window".into())
        .spawn(move || window_thread(receiver))
        .map_err(|err| err.to_string())?;
    let _ = SLOT.set(sender.clone());
    Ok(sender)
}

#[cfg_attr(target_os = "macos", allow(dead_code))]
fn window_thread(receiver: std::sync::mpsc::Receiver<ViewRequest>) {
    let mut event_loop = match player_event_loop() {
        Ok(event_loop) => event_loop,
        Err(err) => {
            let message = err.to_string();
            while let Ok(request) = receiver.recv() {
                let _ = request.proxy.send(Err(message.clone()));
            }
            return;
        }
    };
    while let Ok(request) = receiver.recv() {
        run_view(&mut event_loop, request);
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
            buttons: 0,
            focused: false,
            trace: request.trace,
            clock: TraceClock::default(),
            opened: Instant::now(),
            stamps: request.stamps,
            title: request.title,
            shortcuts: None,
            modifiers: ModifiersState::empty(),
            cursor: (0.0, 0.0),
            letterbox: crate::place(crate::ScaleMode::Fit, 0, 0, 1, 1),
            view: request.view,
            fitted: (0, 0),
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
) {
    loop {
        tokio::time::sleep(Duration::from_millis(30)).await;
        if stop.load(Ordering::SeqCst) {
            pads_stop.store(true, Ordering::SeqCst);
            let _ = proxy.send_event(());
            break;
        }
        let ready = frames.lock().map(|slot| slot.is_some()).unwrap_or(false);
        let titled = title.lock().map(|slot| !slot.is_empty()).unwrap_or(false);
        if ready || titled {
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
    let (audio_tx, audio_rx) = std::sync::mpsc::channel::<AudioChunk>();
    let playback = thread::Builder::new()
        .name("etherquic-play-playback".into())
        .spawn({
            let output = crate::snapshot(&connect.view).output_device;
            move || play_audio(audio_rx, output)
        })?;
    // One slot. The gate keeps the latest pointer and every key, and keys leave first.
    let (input_tx, input_rx) = mpsc::channel::<Vec<Event>>(1);
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
    let dial_stop = Arc::clone(&stop);
    let dial_status = Arc::clone(&status);
    let trace_input = connect.trace_input;
    let stamps = Arc::new(Mutex::new(HashMap::<u64, Instant>::new()));
    let title = Arc::new(Mutex::new(String::new()));
    let dial_stamps = Arc::clone(&stamps);
    let dial_title = Arc::clone(&title);
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
        proxy: proxy_tx,
        done: done_tx,
    };
    let wake_frames = frames.clone();
    let wake_stop = Arc::clone(&stop);
    let wake_pads = Arc::clone(&pads_stop);
    let wake_title = Arc::clone(&title);
    #[cfg(target_os = "macos")]
    {
        // The window runs on this thread, so the waker picks up the proxy itself.
        rt.spawn(async move {
            let proxy = match tokio::task::spawn_blocking(move || proxy_rx.recv()).await {
                Ok(Ok(Ok(proxy))) => proxy,
                _ => return,
            };
            wake_window(proxy, wake_frames, wake_stop, wake_pads, wake_title).await;
        });
        run_view_here(request);
    }
    #[cfg(not(target_os = "macos"))]
    {
        window_sender()
            .map_err(view_error)?
            .send(request)
            .map_err(|_| view_error("the picture window stopped"))?;
        let proxy = proxy_rx
            .recv()
            .map_err(|_| view_error("the picture window stopped"))?
            .map_err(view_error)?;
        // The spawn above has no proxy. A second task watches the same slot and Stop.
        rt.spawn(wake_window(proxy, wake_frames, wake_stop, wake_pads, wake_title));
    }
    done_rx
        .recv()
        .map_err(|_| view_error("the picture window stopped"))?
        .map_err(view_error)?;
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
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
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
    let offer = connect.codecs.offer();
    if negotiates {
        let video: Vec<&str> = offer.video.iter().map(|row| row.token()).collect();
        let audio: Vec<&str> = offer.audio.iter().map(|row| row.token()).collect();
        eprintln!("etherquic-play: offer video [{}] audio [{}]", video.join(" "), audio.join(" "));
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
    };
    let mut session = tokio::spawn(async move { play(conn, media, audio_tx, input, acks).await });
    let codec_status = Arc::clone(&status);
    let codec_title = Arc::clone(&title);
    let quiet_stats = connect.trace_input;
    let pictures = tokio::spawn(async move {
        let mut line = crate::SessionLine::new(peer_text.clone(), negotiates);
        while let Some(event) = event_rx.recv().await {
            let show = line.apply(&event);
            if let Some(log) = line.log_line(&event) {
                eprintln!("etherquic-play: {log}");
            }
            if show && !(quiet_stats && matches!(event, etherquic_media::player::PlayerEvent::Stats(_))) {
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
    buttons: u8,
    focused: bool,
    trace: bool,
    clock: TraceClock,
    opened: Instant,
    stamps: Arc<Mutex<HashMap<u64, Instant>>>,
    title: Arc<Mutex<String>>,
    shortcuts: Option<ShortcutHold>,
    modifiers: ModifiersState,
    cursor: (f64, f64),
    letterbox: crate::Placement,
    view: crate::LiveView,
    /// Picture size the window was last fitted to, for "window matches host".
    fitted: (u32, u32),
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
        if self.stop.load(Ordering::SeqCst) {
            event_loop.exit();
            return;
        }
        if self.window.is_some() {
            return;
        }
        let window = Arc::new(
            event_loop
                .create_window(
                    Window::default_attributes()
                        .with_title("etherquic-play")
                        .with_active(true)
                        .with_inner_size(PhysicalSize::new(1280, 720)),
                )
                .expect("window"),
        );
        window.focus_window();
        let context = Context::new(window.clone()).expect("softbuffer context");
        let surface = Surface::new(&context, window.clone()).expect("softbuffer surface");
        window.request_redraw();
        self.window = Some(window);
        self.context = Some(context);
        self.surface = Some(surface);
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => {
                // Destroy the surface before leaving the loop. The compositor
                // treats a close that is not answered as a frozen window.
                self.drop_window();
                event_loop.exit();
            }
            WindowEvent::Focused(focused) => {
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
                self.send_modifiers(modifiers.state());
            }
            WindowEvent::KeyboardInput { event, .. } => {
                if event.repeat || !self.focused {
                    return;
                }
                self.send_key(&event.logical_key, event.state == ElementState::Pressed);
            }
            WindowEvent::CursorMoved { position, .. } => {
                self.cursor = (position.x, position.y);
                self.send_pointer(0, 0);
            }
            WindowEvent::MouseInput { state, button, .. } => {
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
        if self.stop.load(Ordering::SeqCst) {
            event_loop.exit();
            return;
        }
        self.take_frame();
        self.apply_title();
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        if self.stop.load(Ordering::SeqCst) {
            event_loop.exit();
            return;
        }
        self.take_frame();
        self.apply_title();
    }

    fn exiting(&mut self, _event_loop: &ActiveEventLoop) {
        // Drop the window before winit flushes this run, so the next connect
        // can create another one on the same loop.
        self.drop_window();
    }
}

impl App {
    fn drop_window(&mut self) {
        // The surface has to go first. Shortcut release talks to the session
        // bus and must not run while the compositor is still waiting for close.
        self.surface.take();
        self.context.take();
        self.window.take();
        self.shortcuts.take();
    }

    fn draw(&mut self, event_loop: &ActiveEventLoop) {
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
            let place = crate::place(scale, picture.width, picture.height, size.width, size.height);
            self.letterbox = place;
            let mut view = vec![0u32; (place.view_w as usize) * (place.view_h as usize)];
            blit_rgb(picture, &mut view, place.view_w, place.view_h);
            for y in 0..place.h {
                let row = ((place.crop_y + y) * place.view_w + place.crop_x) as usize;
                for x in 0..place.w {
                    let dst = ((place.dst_y + y) * size.width + place.dst_x + x) as usize;
                    if dst < buffer.len() {
                        buffer[dst] = view[row + x as usize];
                    }
                }
            }
        }
        let _ = buffer.present();
    }

    fn take_frame(&mut self) {
        let next = self.frames.lock().ok().and_then(|mut slot| slot.take());
        if let Some(picture) = next {
            let size = (picture.width, picture.height);
            self.picture = Some(picture);
            if let Some(window) = &self.window {
                let options = crate::snapshot(&self.view);
                if options.size_match == crate::SizeMatch::WindowToHost && size != self.fitted {
                    self.fitted = size;
                    let _ = window.request_inner_size(PhysicalSize::new(size.0, size.1));
                }
                window.request_redraw();
            }
        } else if let Some(window) = &self.window {
            // A scale change made in the console shows without a new frame.
            let scale = crate::snapshot(&self.view).scale;
            if let Some(picture) = &self.picture {
                let size = window.inner_size();
                let place = crate::place(scale, picture.width, picture.height, size.width, size.height);
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

    fn send_key(&mut self, key: &Key, down: bool) {
        if !crate::snapshot(&self.view).send_keyboard {
            return;
        }
        let Some(code) = keysym(key) else {
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

fn keysym(key: &Key) -> Option<u32> {
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

fn named_keysym(key: NamedKey) -> Option<u32> {
    Some(match key {
        NamedKey::Escape => 0xff1b,
        NamedKey::Space => 0x20,
        NamedKey::Enter => 0xff0d,
        NamedKey::Backspace => 0xff08,
        NamedKey::Tab => 0xff09,
        NamedKey::Delete => 0xffff,
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
    use super::*;

    #[test]
    fn the_picture_window_can_close_and_open_again() {
        if std::env::var_os("WAYLAND_DISPLAY").is_none() && std::env::var_os("DISPLAY").is_none() {
            return;
        }
        for _ in 0..2 {
            let (proxy_tx, proxy_rx) = std::sync::mpsc::channel();
            let (done_tx, done_rx) = std::sync::mpsc::channel();
            let stop = Arc::new(AtomicBool::new(false));
            let close = Arc::clone(&stop);
            window_sender()
                .expect("window thread")
                .send(ViewRequest {
                    frames: Arc::new(Mutex::new(None)),
                    input: Arc::new(Gate::new()),
                    stop: Arc::clone(&stop),
                    trace: false,
                    view: crate::live(crate::ViewOptions::default()),
                    stamps: Arc::new(Mutex::new(HashMap::new())),
                    title: Arc::new(Mutex::new(String::new())),
                    proxy: proxy_tx,
                    done: done_tx,
                })
                .expect("send");
            let proxy = proxy_rx
                .recv_timeout(Duration::from_secs(5))
                .expect("proxy")
                .expect("event loop");
            // Let resumed create the window, then close it the way Stop does.
            thread::spawn(move || {
                thread::sleep(Duration::from_millis(400));
                close.store(true, Ordering::SeqCst);
                let _ = proxy.send_event(());
            });
            done_rx
                .recv_timeout(Duration::from_secs(5))
                .expect("done")
                .expect("run");
        }
    }
}
