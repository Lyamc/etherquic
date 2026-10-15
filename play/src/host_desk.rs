//! Capture the primary monitor and its audio, and inject player input.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{Sample, SampleFormat};
use enigo::{Axis, Button, Coordinate, Direction, Enigo, Key, Keyboard, Mouse, Settings};
use etherquic::{install_pure_crypto, l2_server_endpoint, udp_accept, udp_listeners, L2Net};
use etherquic_input::{Body, Buttons, Event, KeySource};
use tokio::sync::mpsc;
use xcap::Monitor;

use crate::pad_inject::Pads;
use crate::{
    desktop_audio, encode_picture, fit_rgba, format_mac, pad_report, place_pointer, serve,
    AudioChunk, DesktopAudio, PictureCodec, PointerPlace, Status,
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
    pub fps: u32,
    pub cert: PathBuf,
    pub key: PathBuf,
    /// Desktop audio source by device name. `None` picks one as before.
    pub audio_device: Option<String>,
}

/// Share this desktop and block until `stop` is set or the player leaves.
///
/// `up` is set once the listener exists. `peers` is 1 while that player is connected.
pub fn host_desktop(
    net: Arc<L2Net>,
    host: HostSession,
    stop: Arc<AtomicBool>,
    up: Arc<AtomicBool>,
    peers: Arc<AtomicUsize>,
    status: Status,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(listen(net, host, stop, up, peers, status))
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
    });
    let (video_tx, video_rx) = mpsc::channel(2);
    let (audio_tx, audio_rx) = mpsc::channel(8);
    let (input_tx, mut input_rx) = mpsc::channel::<Vec<Event>>(32);
    let mut video_rx = Some(video_rx);
    let mut audio_rx = Some(audio_rx);

    let capture_stop = Arc::clone(&stop);
    let capture_screen = screen.clone();
    let max_width = host.max_width.max(2);
    let fps = host.fps.max(1);
    let mut capture = Some(
        thread::Builder::new()
            .name("etherquic-play-capture".into())
            .spawn(move || capture_loop(capture_stop, capture_screen, video_tx, max_width, fps))?,
    );
    let audio_stop = Arc::clone(&stop);
    let audio_device = host.audio_device.clone();
    let mut audio = Some(
        thread::Builder::new()
            .name("etherquic-play-audio".into())
            .spawn(move || {
                if let Err(error) = capture_audio(audio_stop, audio_tx, audio_device.as_deref()) {
                    eprintln!("etherquic-play: audio capture stopped: {error}");
                }
            })?,
    );

    let endpoint =
        match l2_server_endpoint(net, host.local_mac, host.service_id, &host.cert, &host.key) {
            Ok(endpoint) => endpoint,
            Err(error) => {
                release_host(
                    &stop,
                    &up,
                    &peers,
                    &mut video_rx,
                    &mut audio_rx,
                    &mut capture,
                    &mut audio,
                );
                return Err(error.into());
            }
        };
    up.store(true, Ordering::SeqCst);
    let udp = udp_listeners(host.service_id, &host.cert, &host.key);
    let udp_note = udp.note;
    let udp_v4 = udp.v4;
    let udp_v6 = udp.v6;
    let mac = format_mac(host.local_mac);
    status(&format!(
        "Sharing this desktop as {mac} on service {}. {udp_note} Waiting for one player.",
        host.service_id
    ));

    let incoming = tokio::select! {
        incoming = endpoint.accept() => incoming,
        incoming = udp_accept(udp_v4.as_ref()) => incoming,
        incoming = udp_accept(udp_v6.as_ref()) => incoming,
        _ = until_stop(&stop) => {
            release_host(
                &stop,
                &up,
                &peers,
                &mut video_rx,
                &mut audio_rx,
                &mut capture,
                &mut audio,
            );
            status("Host stopped.");
            return Ok(());
        }
    };
    let Some(incoming) = incoming else {
        release_host(
            &stop,
            &up,
            &peers,
            &mut video_rx,
            &mut audio_rx,
            &mut capture,
            &mut audio,
        );
        return Err("endpoint closed before a player connected".into());
    };
    let conn = match incoming.await {
        Ok(conn) => conn,
        Err(error) => {
            release_host(
                &stop,
                &up,
                &peers,
                &mut video_rx,
                &mut audio_rx,
                &mut capture,
                &mut audio,
            );
            return Err(error.into());
        }
    };
    peers.store(1, Ordering::SeqCst);
    status(&format!(
        "Player connected ({}). This desktop is on their screen.",
        conn.remote_address()
    ));

    let inject_screen = screen.clone();
    let (ack_tx, ack_rx) = mpsc::channel(64);
    // macOS input injection holds a CGEventSource, which is not Send, so it
    // gets a blocking thread there; elsewhere it stays an async task.
    #[cfg(target_os = "macos")]
    let inject = tokio::task::spawn_blocking(move || {
        let mut hands = Hands::open();
        while let Some(events) = input_rx.blocking_recv() {
            let width = inject_screen.width.load(Ordering::Relaxed).max(1);
            let height = inject_screen.height.load(Ordering::Relaxed).max(1);
            hands.apply(&events, width, height, &ack_tx);
        }
    });
    #[cfg(not(target_os = "macos"))]
    let inject = tokio::spawn(async move {
        let mut hands = Hands::open();
        while let Some(events) = input_rx.recv().await {
            let width = inject_screen.width.load(Ordering::Relaxed).max(1);
            let height = inject_screen.height.load(Ordering::Relaxed).max(1);
            hands.apply(&events, width, height, &ack_tx);
        }
    });

    let session = serve(
        conn,
        video_rx.take().expect("video receiver"),
        audio_rx.take().expect("audio receiver"),
        input_tx,
        Some(ack_rx),
    );
    let asked = tokio::select! {
        result = session => {
            let asked = stop.load(Ordering::SeqCst);
            if let Err(error) = result {
                inject.abort();
                release_host(
                    &stop,
                    &up,
                    &peers,
                    &mut video_rx,
                    &mut audio_rx,
                    &mut capture,
                    &mut audio,
                );
                return Err(error.into());
            }
            asked
        }
        _ = until_stop(&stop) => true,
    };
    inject.abort();
    release_host(
        &stop,
        &up,
        &peers,
        &mut video_rx,
        &mut audio_rx,
        &mut capture,
        &mut audio,
    );
    if asked {
        status("Host stopped.");
    } else {
        status("The player left.");
    }
    Ok(())
}

fn release_host(
    stop: &AtomicBool,
    up: &AtomicBool,
    peers: &AtomicUsize,
    video: &mut Option<mpsc::Receiver<Vec<u8>>>,
    audio_rx: &mut Option<mpsc::Receiver<AudioChunk>>,
    capture: &mut Option<JoinHandle<()>>,
    audio: &mut Option<JoinHandle<()>>,
) {
    // Drop the receivers first. A full queue would otherwise block the join.
    video.take();
    audio_rx.take();
    stop.store(true, Ordering::SeqCst);
    up.store(false, Ordering::SeqCst);
    peers.store(0, Ordering::SeqCst);
    if let Some(handle) = capture.take() {
        let _ = handle.join();
    }
    if let Some(handle) = audio.take() {
        let _ = handle.join();
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
}

fn capture_loop(
    stop: Arc<AtomicBool>,
    screen: Arc<Screen>,
    video: mpsc::Sender<Vec<u8>>,
    max_width: u32,
    fps: u32,
) {
    let period = Duration::from_millis((1000 / fps.max(1) as u64).max(1));
    let mut frames = 0u64;
    let mut logged = Instant::now();
    while !stop.load(Ordering::Relaxed) {
        let started = Instant::now();
        match one_frame(&screen, max_width) {
            Ok(jpeg) => {
                frames += 1;
                if logged.elapsed() >= Duration::from_secs(2) {
                    eprintln!(
                        "etherquic-play: {frames} frames, last jpeg {} bytes",
                        jpeg.len()
                    );
                    logged = Instant::now();
                }
                if video.blocking_send(jpeg).is_err() {
                    break;
                }
            }
            Err(error) => {
                eprintln!("etherquic-play: capture: {error}");
                thread::sleep(Duration::from_secs(1));
                continue;
            }
        }
        let spent = started.elapsed();
        if spent < period {
            thread::sleep(period - spent);
        }
    }
}

fn one_frame(screen: &Screen, max_width: u32) -> Result<Vec<u8>, String> {
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
    let image = monitor.capture_image().map_err(|error| error.to_string())?;
    let picture = fit_rgba(image.as_raw(), image.width(), image.height(), max_width)
        .map_err(|error| error.to_string())?;
    encode_picture(PictureCodec::Mjpeg, &picture, 40).map_err(|error| error.to_string())
}

fn capture_audio(
    stop: Arc<AtomicBool>,
    audio: mpsc::Sender<AudioChunk>,
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
        if audio.blocking_send(chunk).is_err() {
            break;
        }
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

fn key_from_keysym(code: u32) -> Option<Key> {
    Some(match code {
        0xff1b => Key::Escape,
        0xff0d | 0xff8d => Key::Return,
        0xff08 => Key::Backspace,
        0xff09 => Key::Tab,
        0xffff => Key::Delete,
        0xff51 => Key::LeftArrow,
        0xff52 => Key::UpArrow,
        0xff53 => Key::RightArrow,
        0xff54 => Key::DownArrow,
        0xff50 => Key::Home,
        0xff57 => Key::End,
        0xff55 => Key::PageUp,
        0xff56 => Key::PageDown,
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
        code if code < 0x80 => Key::Unicode(char::from_u32(code)?),
        _ => return None,
    })
}
