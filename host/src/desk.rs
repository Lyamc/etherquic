//! Console-session capture, loopback audio, and input injection.
//!
//! The service starts this with `--desk` on the interactive desktop. Session 0
//! cannot see that desktop, so GDI, WASAPI loopback, and `SendInput` stay here.
//! Desktop duplication, when `desk.conf` leaves it on, only keeps DWM composing.
//! The JPEG is still the GDI copy.
//!
//! Capture and injection follow the input desktop. After idle the console can
//! be on the screen saver's desktop or on `Winlogon`, and a thread left on
//! `Default` sees a frozen picture and has every `SendInput` refused. When a
//! player attaches, the service sends `Wake`: the display is woken and held on,
//! and the screen saver is ended, until `Release`.

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

struct Screen {
    width: AtomicU32,
    height: AtomicU32,
    frame_w: AtomicU32,
    frame_h: AtomicU32,
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
        frame_w: AtomicU32::new(0),
        frame_h: AtomicU32::new(0),
    });
    let frames = Arc::new(Mutex::new(None));
    let cursors = Arc::new(Mutex::new(None::<Vec<u8>>));
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
    let capture_cursors = Arc::clone(&cursors);
    let capture = thread::Builder::new()
        .name("etherquic-capture".into())
        .spawn(move || {
            capture_loop(
                capture_stop,
                capture_screen,
                capture_frames,
                capture_cursors,
                capture_notes,
            )
        })
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
    let write_cursors = Arc::clone(&cursors);
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
                write_cursors,
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
    cursors: Arc<Mutex<Option<Vec<u8>>>>,
    notes: Arc<Mutex<VecDeque<String>>>,
) {
    let mut cursor = etherquic::cursor::Watch::new();
    link::init_com();
    let mut capture = desktop::ScreenCapture::open();
    let mut logged_mode = String::new();
    note_mode(&notes, &capture, &mut logged_mode);
    let fps = desktop::display_hz().unwrap_or(proto::CAPTURE_FPS);
    let period = Duration::from_millis((1000 / u64::from(fps.max(1))).max(1));
    note(&notes, format!("capture {fps} fps"));
    let mut logged: Option<(u32, u32)> = None;
    let mut logged_desktop = capture.desktop().to_string();
    let mut last_error = String::new();
    let mut repeats = 0u32;
    while !stop.load(Ordering::SeqCst) {
        let started = Instant::now();
        let result = one_frame(&screen, &mut capture);
        note_mode(&notes, &capture, &mut logged_mode);
        if capture.desktop() != logged_desktop {
            note(&notes, format!("capture desktop {}", capture.desktop()));
            logged_desktop = capture.desktop().to_string();
        }
        match result {
            Ok((raw, width, height)) => {
                if logged != Some((width, height)) {
                    note(
                        &notes,
                        format!("frame {width}x{height} raw {} bytes", raw.len()),
                    );
                    logged = Some((width, height));
                }
                if repeats > 1 {
                    note(&notes, format!("capture recovered after {repeats} errors"));
                }
                last_error.clear();
                repeats = 0;
                publish_frame(&frames, raw);
            }
            Err(error) => {
                // One line per new error, then one a minute. A headless idle
                // console used to write this once a second for hours.
                repeats += 1;
                if error != last_error || repeats % 60 == 0 {
                    note(&notes, format!("capture: {error} (x{repeats})"));
                    last_error = error;
                }
                thread::sleep(Duration::from_secs(1));
                continue;
            }
        }
        pace(started, period, stop, || {
            publish_cursor(&mut cursor, &screen, &cursors)
        });
    }
}

fn publish_cursor(
    cursor: &mut etherquic::cursor::Watch,
    screen: &Screen,
    cursors: &Mutex<Option<Vec<u8>>>,
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
        .map_to_frame(0, 0, capture_w, capture_h, frame_w, frame_h)
        .encode();
    *cursors.lock().unwrap_or_else(|poison| poison.into_inner()) = Some(bytes);
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
    screen.frame_w.store(picture.width, Ordering::Relaxed);
    screen.frame_h.store(picture.height, Ordering::Relaxed);
    Ok((picture.raw_payload(), picture.width, picture.height))
}

/// Silence the loopback owes the player after `gap` with no packet.
///
/// WASAPI loopback delivers nothing at all while no app renders audio, which
/// is the normal state of an idle console. The player then sees an audio
/// stream with no records. Silence keeps the stream moving. A gap is at most
/// 200 ms of frames so a stall does not become one huge record.
pub fn silence_frames(rate: u32, gap: Duration) -> usize {
    let gap = gap.min(Duration::from_millis(200));
    (u128::from(rate) * gap.as_micros() / 1_000_000) as usize
}

/// Emit silence after this long without a loopback packet.
const SILENCE_AFTER: Duration = Duration::from_millis(60);
/// Reopen the default output after this long without a real packet. An
/// output that went away while the display slept does not report an error.
const REOPEN_AFTER: Duration = Duration::from_secs(10);

fn audio_loop(
    stop: &AtomicBool,
    audio: SyncSender<AudioChunk>,
    notes: Arc<Mutex<VecDeque<String>>>,
) {
    link::init_com();
    let mut loopback = None;
    let mut last_error = String::new();
    let mut logged_format = None;
    let mut last_packet = Instant::now();
    let mut last_sent = Instant::now();
    while !stop.load(Ordering::SeqCst) {
        if loopback.is_none() {
            match desktop::Loopback::open() {
                Ok(next) => {
                    let format = (next.rate(), next.channels());
                    if logged_format != Some(format) {
                        note(
                            &notes,
                            format!(
                                "audio capture {} {} Hz {} ch",
                                next.name(),
                                format.0,
                                format.1
                            ),
                        );
                        logged_format = Some(format);
                    }
                    last_error.clear();
                    last_packet = Instant::now();
                    loopback = Some(next);
                }
                Err(error) => {
                    if error != last_error {
                        note(&notes, format!("audio: {error}"));
                        last_error = error;
                    }
                    thread::sleep(Duration::from_secs(1));
                    continue;
                }
            }
        }
        let Some(open) = loopback.as_mut() else {
            continue;
        };
        match open.poll() {
            Ok(Some(chunk)) => {
                last_packet = Instant::now();
                last_sent = last_packet;
                let _ = audio.try_send(chunk);
            }
            Ok(None) => {
                let gap = last_sent.elapsed();
                if gap >= SILENCE_AFTER {
                    let frames = silence_frames(open.rate(), gap);
                    let channels = open.channels().max(1);
                    let _ = audio.try_send(AudioChunk {
                        sample_rate: open.rate(),
                        channels,
                        pcm: vec![0; frames * usize::from(channels)],
                    });
                    last_sent = Instant::now();
                }
                if last_packet.elapsed() >= REOPEN_AFTER {
                    // Picks up a new default output, or the same one powered back up.
                    loopback = None;
                    continue;
                }
                thread::sleep(Duration::from_millis(10));
            }
            Err(error) => {
                note(&notes, format!("audio: {error}"));
                loopback = None;
                thread::sleep(Duration::from_millis(200));
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
    cursors: Arc<Mutex<Option<Vec<u8>>>>,
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
        let raw = frames
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .take();
        if let Some(raw) = raw {
            progress = true;
            if pipe.write_message(&Message::Raw(raw), stop).is_err() {
                failed.store(true, Ordering::SeqCst);
                return;
            }
        }
        let cursor = cursors
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .take();
        if let Some(bytes) = cursor {
            progress = true;
            if pipe.write_message(&Message::Cursor(bytes), stop).is_err() {
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
    let mut hands = Hands {
        buttons: 0,
        warned: None,
        followed: Instant::now() - Duration::from_secs(1),
        desktop: desktop::thread_desktop_name(),
    };
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
            Ok(Some(Message::Wake)) => {
                // This thread injects, so it holds the display and owns the desktop move.
                desktop::hold_display(true);
                hands.follow(&notes);
                match desktop::wake_display() {
                    Ok(()) => note(
                        &notes,
                        format!(
                            "wake: display on, input desktop {}",
                            desktop::thread_desktop_name()
                        ),
                    ),
                    Err(error) => note(&notes, format!("wake: {error}")),
                }
            }
            Ok(Some(Message::Release)) => {
                desktop::hold_display(false);
                note(&notes, "release: display may idle".to_string());
            }
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
        event.time_us,
        kind,
        code,
        result,
        detail,
    ))
}

struct Hands {
    buttons: u8,
    /// Last injection error noted. Cleared by the next success.
    warned: Option<String>,
    /// Last time this thread checked the input desktop.
    followed: Instant,
    desktop: String,
}

/// How often the injecting thread checks the input desktop between failures.
const FOLLOW_EVERY: Duration = Duration::from_millis(250);

impl Hands {
    fn follow(&mut self, notes: &Mutex<VecDeque<String>>) {
        self.followed = Instant::now();
        match desktop::follow_input_desktop() {
            Ok(Some(name)) => {
                note(notes, format!("input desktop {name}"));
                self.desktop = name;
            }
            Ok(None) => {}
            Err(error) => {
                if self.warned.as_deref() != Some(error.as_str()) {
                    note(notes, format!("input desktop: {error}"));
                    self.warned = Some(error);
                }
            }
        }
    }

    fn apply(
        &mut self,
        events: &[Event],
        screen: &Screen,
        notes: &Mutex<VecDeque<String>>,
        acks: &Mutex<VecDeque<Vec<u8>>>,
    ) {
        if self.followed.elapsed() >= FOLLOW_EVERY {
            self.follow(notes);
        }
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
            match outcome {
                Err(error) => {
                    // Once per distinct error, again after input worked in between.
                    if self.warned.as_deref() != Some(error.as_str()) {
                        note(notes, format!("inject: {error} (desktop {})", self.desktop));
                        self.warned = Some(error);
                    }
                }
                Ok(true) if self.warned.is_some() => {
                    note(
                        notes,
                        format!("inject: working again on desktop {}", self.desktop),
                    );
                    self.warned = None;
                }
                Ok(_) => {}
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
            Body::Delta { .. }
            | Body::Gamepad { .. }
            | Body::Sync { .. }
            | Body::Unknown { .. } => {
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

#[cfg(test)]
mod tests {
    use super::silence_frames;
    use std::time::Duration;

    #[test]
    fn idle_loopback_owes_silence_for_the_gap() {
        assert_eq!(silence_frames(48_000, Duration::from_millis(100)), 4_800);
        assert_eq!(silence_frames(44_100, Duration::from_millis(60)), 2_646);
        // Capped so a long stall is not one huge record.
        assert_eq!(silence_frames(48_000, Duration::from_secs(5)), 9_600);
        assert_eq!(silence_frames(48_000, Duration::ZERO), 0);
    }
}
