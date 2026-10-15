//! Console-session capture, loopback audio, and input injection.
//!
//! The service starts this with `--desk` on the interactive desktop. Session 0
//! cannot see that desktop, so GDI, WASAPI loopback, and `SendInput` stay here.
//! Desktop duplication, when `desk.conf` leaves it on, only keeps DWM composing.
//! The JPEG is still the GDI copy.

#![forbid(unsafe_code)]

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::{sync_channel, Receiver, SyncSender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use etherquic_input::{decode, Body, Buttons, Event, KeySource};

use crate::desktop;
use crate::link::{self, Pipe};
use crate::proto::{self, fit_rgba, AudioChunk, Message};

const MAX_WIDTH: u32 = 1280;
const FPS: u32 = 10;

struct Screen {
    width: AtomicU32,
    height: AtomicU32,
}

pub fn run(stop: &'static AtomicBool) -> Result<(), String> {
    link::dpi_aware();
    let pipe = Pipe::connect_client(stop)?;
    let reader_pipe = pipe.duplicate()?;
    let writer_pipe = pipe.duplicate()?;
    drop(pipe);

    let screen = Arc::new(Screen {
        width: AtomicU32::new(0),
        height: AtomicU32::new(0),
    });
    let frames = Arc::new(Mutex::new(None));
    let (audio_tx, audio_rx) = sync_channel(16);
    let notes = Arc::new(Mutex::new(VecDeque::new()));
    let acks = Arc::new(Mutex::new(VecDeque::<Vec<u8>>::new()));
    let failed = Arc::new(AtomicBool::new(false));
    note(
        &notes,
        format!("desk helper ready ({})", link::account_label()),
    );

    let capture_stop = stop;
    let capture_screen = Arc::clone(&screen);
    let capture_notes = Arc::clone(&notes);
    let capture_frames = Arc::clone(&frames);
    let capture = thread::Builder::new()
        .name("etherquic-capture".into())
        .spawn(move || capture_loop(capture_stop, capture_screen, capture_frames, capture_notes))
        .map_err(|err| err.to_string())?;

    let audio_notes = Arc::clone(&notes);
    let audio = thread::Builder::new()
        .name("etherquic-audio".into())
        .spawn(move || audio_loop(stop, audio_tx, audio_notes))
        .map_err(|err| err.to_string())?;

    let write_failed = Arc::clone(&failed);
    let write_frames = Arc::clone(&frames);
    let write_notes = Arc::clone(&notes);
    let write_acks = Arc::clone(&acks);
    let writer = thread::Builder::new()
        .name("etherquic-desk-write".into())
        .spawn(move || {
            write_loop(
                writer_pipe,
                stop,
                write_frames,
                audio_rx,
                write_notes,
                write_acks,
                write_failed,
            )
        })
        .map_err(|err| err.to_string())?;

    let read_failed = Arc::clone(&failed);
    let reader = thread::Builder::new()
        .name("etherquic-desk-read".into())
        .spawn(move || read_loop(reader_pipe, stop, screen, notes, acks, read_failed))
        .map_err(|err| err.to_string())?;

    while !stop.load(Ordering::SeqCst) && !failed.load(Ordering::SeqCst) {
        thread::sleep(Duration::from_millis(50));
    }
    let _ = capture.join();
    let _ = audio.join();
    let _ = writer.join();
    let _ = reader.join();
    Ok(())
}

fn note(notes: &Mutex<VecDeque<String>>, text: String) {
    eprintln!("etherquic-host: {text}");
    let mut queue = notes.lock().unwrap_or_else(|poison| poison.into_inner());
    if queue.len() >= 32 {
        queue.pop_front();
    }
    queue.push_back(text);
}

fn publish_frame(frames: &Mutex<Option<Vec<u8>>>, jpeg: Vec<u8>) {
    *frames.lock().unwrap_or_else(|poison| poison.into_inner()) = Some(jpeg);
}

fn capture_loop(
    stop: &AtomicBool,
    screen: Arc<Screen>,
    frames: Arc<Mutex<Option<Vec<u8>>>>,
    notes: Arc<Mutex<VecDeque<String>>>,
) {
    link::init_com();
    let mut capture = desktop::ScreenCapture::open();
    let mut logged_mode = String::new();
    note_mode(&notes, &capture, &mut logged_mode);
    let period = Duration::from_millis((1000 / u64::from(FPS.max(1))).max(1));
    let mut logged: Option<(u32, u32)> = None;
    while !stop.load(Ordering::SeqCst) {
        let started = Instant::now();
        let result = one_frame(&screen, &mut capture);
        note_mode(&notes, &capture, &mut logged_mode);
        match result {
            Ok((jpeg, width, height)) => {
                if logged != Some((width, height)) {
                    note(
                        &notes,
                        format!("frame {width}x{height} jpeg {} bytes", jpeg.len()),
                    );
                    logged = Some((width, height));
                }
                publish_frame(&frames, jpeg);
            }
            Err(error) => {
                note(&notes, format!("capture: {error}"));
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

fn note_mode(
    notes: &Mutex<VecDeque<String>>,
    capture: &desktop::ScreenCapture,
    logged: &mut String,
) {
    if capture.mode() != logged.as_str() {
        note(notes, format!("capture {}", capture.mode()));
        *logged = capture.mode().to_string();
    }
}

fn one_frame(
    screen: &Screen,
    capture: &mut desktop::ScreenCapture,
) -> Result<(Vec<u8>, u32, u32), String> {
    let (rgba, width, height) = capture.frame()?;
    screen.width.store(width.max(1), Ordering::Relaxed);
    screen.height.store(height.max(1), Ordering::Relaxed);
    let picture = fit_rgba(&rgba, width, height, MAX_WIDTH)?;
    let jpeg = proto::encode_jpeg(&picture, 40)?;
    Ok((jpeg, picture.width, picture.height))
}

fn audio_loop(
    stop: &AtomicBool,
    audio: SyncSender<AudioChunk>,
    notes: Arc<Mutex<VecDeque<String>>>,
) {
    link::init_com();
    let mut loopback = match desktop::Loopback::open() {
        Ok(loopback) => loopback,
        Err(error) => {
            note(&notes, format!("audio: {error}"));
            return;
        }
    };
    note(
        &notes,
        format!(
            "audio capture {} {} Hz {} ch",
            loopback.name(),
            loopback.rate(),
            loopback.channels()
        ),
    );
    while !stop.load(Ordering::SeqCst) {
        match loopback.poll() {
            Ok(Some(chunk)) => {
                let _ = audio.try_send(chunk);
            }
            Ok(None) => thread::sleep(Duration::from_millis(10)),
            Err(error) => {
                note(&notes, format!("audio: {error}"));
                thread::sleep(Duration::from_secs(1));
                match desktop::Loopback::open() {
                    Ok(next) => loopback = next,
                    Err(error) => note(&notes, format!("audio: {error}")),
                }
            }
        }
    }
}

fn write_loop(
    pipe: Pipe,
    stop: &AtomicBool,
    frames: Arc<Mutex<Option<Vec<u8>>>>,
    audio: Receiver<AudioChunk>,
    notes: Arc<Mutex<VecDeque<String>>>,
    acks: Arc<Mutex<VecDeque<Vec<u8>>>>,
    failed: Arc<AtomicBool>,
) {
    while !stop.load(Ordering::SeqCst) {
        let mut progress = false;
        let pending_acks: Vec<Vec<u8>> = {
            let mut queue = acks.lock().unwrap_or_else(|poison| poison.into_inner());
            queue.drain(..).collect()
        };
        for bytes in pending_acks {
            progress = true;
            if pipe.write_message(&Message::Ack(bytes), stop).is_err() {
                failed.store(true, Ordering::SeqCst);
                return;
            }
        }
        let pending_notes: Vec<String> = {
            let mut queue = notes.lock().unwrap_or_else(|poison| poison.into_inner());
            queue.drain(..).collect()
        };
        for text in pending_notes {
            progress = true;
            if pipe.write_message(&Message::Note(text), stop).is_err() {
                failed.store(true, Ordering::SeqCst);
                return;
            }
        }
        while let Ok(chunk) = audio.try_recv() {
            progress = true;
            let Ok(payload) = chunk.encode() else {
                continue;
            };
            if pipe.write_message(&Message::Pcm(payload), stop).is_err() {
                failed.store(true, Ordering::SeqCst);
                return;
            }
        }
        let jpeg = frames
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .take();
        if let Some(jpeg) = jpeg {
            progress = true;
            if pipe.write_message(&Message::Jpeg(jpeg), stop).is_err() {
                failed.store(true, Ordering::SeqCst);
                return;
            }
        }
        if !progress {
            thread::sleep(Duration::from_millis(5));
        }
    }
}

fn read_loop(
    pipe: Pipe,
    stop: &AtomicBool,
    screen: Arc<Screen>,
    notes: Arc<Mutex<VecDeque<String>>>,
    acks: Arc<Mutex<VecDeque<Vec<u8>>>>,
    failed: Arc<AtomicBool>,
) {
    let mut hands = Hands { buttons: 0, warned: false };
    loop {
        if stop.load(Ordering::SeqCst) {
            return;
        }
        match pipe.read_message(stop) {
            Ok(None) => {
                failed.store(true, Ordering::SeqCst);
                return;
            }
            Ok(Some(Message::Eqin(bytes))) => match decode(&bytes) {
                Ok(events) => hands.apply(&events, &screen, &notes, &acks),
                Err(error) => note(&notes, format!("input: {error}")),
            },
            Ok(Some(_)) => {}
            Err(error) => {
                note(&notes, format!("desk read: {error}"));
                failed.store(true, Ordering::SeqCst);
                return;
            }
        }
    }
}

fn confirm(event: &Event, outcome: &Result<bool, String>) -> Option<Vec<u8>> {
    if event.time_us == 0 {
        return None;
    }
    let (kind, code, detail) = match &event.body {
        Body::Key { down, code, .. } => (proto::ACK_KEY, *code, u8::from(*down)),
        Body::Pointer { .. } | Body::Delta { .. } => (proto::ACK_POINTER, 0, 0),
        _ => (proto::ACK_OTHER, 0, 0),
    };
    let result = match outcome {
        Ok(true) => proto::ACK_APPLIED,
        Ok(false) => proto::ACK_IGNORED,
        Err(_) => proto::ACK_FAILED,
    };
    Some(proto::encode_input_ack(
        event.time_us, kind, code, result, detail,
    ))
}

struct Hands {
    buttons: u8,
    warned: bool,
}

impl Hands {
    fn apply(
        &mut self,
        events: &[Event],
        screen: &Screen,
        notes: &Mutex<VecDeque<String>>,
        acks: &Mutex<VecDeque<Vec<u8>>>,
    ) {
        let width = screen.width.load(Ordering::Relaxed).max(1);
        let height = screen.height.load(Ordering::Relaxed).max(1);
        for event in events {
            let outcome = self.one(event, width, height, notes);
            if let Some(bytes) = confirm(event, &outcome) {
                let mut queue = acks.lock().unwrap_or_else(|poison| poison.into_inner());
                if queue.len() >= 64 {
                    queue.pop_front();
                }
                queue.push_back(bytes);
            }
            if let Err(error) = outcome {
                if !self.warned {
                    note(notes, format!("inject: {error}"));
                    self.warned = true;
                }
            }
        }
    }

    fn one(
        &mut self,
        event: &Event,
        width: u32,
        height: u32,
        notes: &Mutex<VecDeque<String>>,
    ) -> Result<bool, String> {
        match &event.body {
            Body::Pointer {
                x,
                y,
                buttons,
                wheel_y,
                wheel_x,
            } => {
                let (px, py) = proto::place_pointer(*x, *y, width, height);
                desktop::pointer(*x, *y)?;
                self.buttons(*buttons)?;
                desktop::wheel(*wheel_y, *wheel_x)?;
                note(notes, format!("applied pointer {px} {py}"));
            }
            Body::Key { down, source, code } => {
                if *source != KeySource::Keysym {
                    return Ok(false);
                }
                if !desktop::key_keysym(*code, *down)? {
                    return Ok(false);
                }
                let state = if *down { "down" } else { "up" };
                note(notes, format!("applied key {code:#x} {state}"));
            }
            Body::Delta { .. } | Body::Gamepad { .. } | Body::Sync { .. } | Body::Unknown { .. } => {
                return Ok(false);
            }
        }
        Ok(true)
    }

    fn buttons(&mut self, next: u8) -> Result<(), String> {
        for bit in [Buttons::LEFT, Buttons::MIDDLE, Buttons::RIGHT] {
            let was = self.buttons & bit != 0;
            let now = next & bit != 0;
            if was == now {
                continue;
            }
            desktop::button(bit, now)?;
        }
        self.buttons = next;
        Ok(())
    }
}
