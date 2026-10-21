//! Keep the desktop helper alive and feed play sessions from its pipe.
//!
//! The helper stays up across players. Capture writes one FrameSlot for every
//! viewer. Audio is dropped when a player is behind. Input is written as EQIN
//! and injected on the console desktop.

#![forbid(unsafe_code)]

use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use etherquic_input::{encode, Event};
use etherquic_media::host::{EncodeHub, FrameSlot, KeyframeBus, Room, SharedEncode};
use tokio::sync::{broadcast, mpsc};

use crate::link::{self, Pipe};
use crate::proto::{AudioChunk, Message, Rgb};
use crate::run::Log;

struct Outbound {
    video: FrameSlot,
    audio: broadcast::Sender<AudioChunk>,
    acks: broadcast::Sender<Vec<u8>>,
    cursor: tokio::sync::watch::Sender<Option<Vec<u8>>>,
}

pub use crate::extras::Picture;

pub struct Desk {
    log: Log,
    stop: Arc<AtomicBool>,
    /// True while at least one player is attached. The input thread tells the helper.
    session: Arc<AtomicBool>,
    peers: Arc<AtomicUsize>,
    frames: FrameSlot,
    audio: broadcast::Sender<AudioChunk>,
    acks: broadcast::Sender<Vec<u8>>,
    cursor_rx: tokio::sync::watch::Receiver<Option<Vec<u8>>>,
    shared: SharedEncode,
    outbound: Arc<Mutex<Option<Outbound>>>,
    input: mpsc::Sender<Vec<Event>>,
    supervisor: Option<JoinHandle<()>>,
    writer: Option<JoinHandle<()>>,
}

impl Desk {
    pub fn spawn(log: &Log, picture: Picture) -> Self {
        // Probe encoders now; the first player should not wait for ffmpeg.
        etherquic_media::caps::warm();
        let stop = Arc::new(AtomicBool::new(false));
        let session = Arc::new(AtomicBool::new(false));
        let peers = Arc::new(AtomicUsize::new(0));
        let generation = Arc::new(AtomicU64::new(0));
        let frames = FrameSlot::new();
        let (audio_tx, _) = broadcast::channel(32);
        let (ack_tx, _) = broadcast::channel(64);
        let (cursor_tx, cursor_rx) = tokio::sync::watch::channel(None);
        let caps = etherquic_media::caps::host_caps();
        let media_log: etherquic_media::Log = {
            let log = log.clone();
            Arc::new(move |line: &str| log.line(line))
        };
        let (fps, display_hz) = crate::media::host_rate();
        let options = etherquic_media::host::HostOptions {
            fps,
            display_hz,
            caps,
            log: media_log,
            cap_kbps: std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0)),
            carrier_tap: Some(etherquic::carrier::open_carrier_tap()),
        };
        let keyframes = KeyframeBus::default();
        let hub = EncodeHub::new(frames.clone(), keyframes.clone(), &options);
        let room = Room::default();
        let shared = SharedEncode {
            keyframes,
            hub: Some(hub),
            room: Some(room),
        };
        let outbound = Arc::new(Mutex::new(Some(Outbound {
            video: frames.clone(),
            audio: audio_tx.clone(),
            acks: ack_tx.clone(),
            cursor: cursor_tx,
        })));
        let pipe_slot = Arc::new(Mutex::new(None));
        let (input_tx, input_rx) = mpsc::channel(64);
        let writer_stop = Arc::clone(&stop);
        let writer_slot = Arc::clone(&pipe_slot);
        let writer_session = Arc::clone(&session);
        let writer_generation = Arc::clone(&generation);
        let writer = thread::Builder::new()
            .name("etherquic-input".into())
            .spawn(move || {
                input_loop(
                    writer_slot,
                    input_rx,
                    writer_stop,
                    writer_session,
                    writer_generation,
                )
            })
            .ok();
        let supervisor = thread::Builder::new()
            .name("etherquic-desk".into())
            .spawn({
                let stop = Arc::clone(&stop);
                let pipe_slot = Arc::clone(&pipe_slot);
                let outbound = Arc::clone(&outbound);
                let generation = Arc::clone(&generation);
                let log = log.clone();
                move || supervise(stop, pipe_slot, outbound, generation, log, picture)
            })
            .ok();
        if writer.is_none() || supervisor.is_none() {
            log.line("desk thread failed to start");
        }
        Self {
            log: log.clone(),
            stop,
            session,
            peers,
            frames,
            audio: audio_tx,
            acks: ack_tx,
            cursor_rx,
            shared,
            outbound,
            input: input_tx,
            supervisor,
            writer,
        }
    }

    pub fn input(&self) -> mpsc::Sender<Vec<Event>> {
        self.input.clone()
    }

    pub fn locked(&self) -> bool {
        self.shared
            .room
            .as_ref()
            .is_some_and(|room| room.locked())
    }

    pub async fn serve_player(&self, conn: quinn::Connection) -> Result<(), String> {
        if self.locked() {
            conn.close(1u32.into(), b"exclusive");
            return Err("exclusive session".into());
        }
        let n = self.peers.fetch_add(1, Ordering::SeqCst) + 1;
        self.session.store(true, Ordering::SeqCst);
        let _guard = Leave(self);
        let log = self.log.clone();
        let media_log: etherquic_media::Log = Arc::new(move |line: &str| log.line(line));
        let _ = n;
        crate::media::serve_shared(
            conn,
            self.frames.clone(),
            subscribe_audio(&self.audio),
            self.input(),
            subscribe_bytes(&self.acks),
            media_log,
            self.cursor_rx.clone(),
            self.shared.clone(),
        )
        .await
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

fn subscribe_bytes(tx: &broadcast::Sender<Vec<u8>>) -> mpsc::Receiver<Vec<u8>> {
    let mut rx = tx.subscribe();
    let (out_tx, out_rx) = mpsc::channel(64);
    tokio::spawn(async move {
        loop {
            match rx.recv().await {
                Ok(bytes) => {
                    if out_tx.send(bytes).await.is_err() {
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

struct Leave<'a>(&'a Desk);

impl Drop for Leave<'_> {
    fn drop(&mut self) {
        let left = self.0.peers.fetch_sub(1, Ordering::SeqCst).saturating_sub(1);
        self.0.session.store(left > 0, Ordering::SeqCst);
    }
}

impl Drop for Desk {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(handle) = self.supervisor.take() {
            let _ = handle.join();
        }
        if let Some(handle) = self.writer.take() {
            let _ = handle.join();
        }
    }
}

fn supervise(
    stop: Arc<AtomicBool>,
    pipe_slot: Arc<Mutex<Option<Pipe>>>,
    outbound: Arc<Mutex<Option<Outbound>>>,
    generation: Arc<AtomicU64>,
    log: Log,
    picture: Picture,
) {
    let mut last_error = String::new();
    let mut logged_at = Instant::now() - Duration::from_secs(60);
    while !stop.load(Ordering::SeqCst) {
        match run_once(
            Arc::clone(&stop),
            Arc::clone(&pipe_slot),
            Arc::clone(&outbound),
            &generation,
            &log,
            &picture,
        ) {
            Ok(()) => break,
            Err(error) => {
                if error != last_error || logged_at.elapsed() > Duration::from_secs(30) {
                    log.line(&format!("desk: {error}"));
                    last_error = error;
                    logged_at = Instant::now();
                }
                thread::sleep(Duration::from_secs(1));
            }
        }
    }
}

fn run_once(
    stop: Arc<AtomicBool>,
    pipe_slot: Arc<Mutex<Option<Pipe>>>,
    outbound: Arc<Mutex<Option<Outbound>>>,
    generation: &AtomicU64,
    log: &Log,
    picture: &Picture,
) -> Result<(), String> {
    if stop.load(Ordering::SeqCst) {
        return Ok(());
    }
    let pipe = Pipe::listen()?;
    let child = link::launch()?;
    if let Err(error) = pipe.accept(&stop) {
        return Err(error);
    }
    log.line(&format!(
        "desk helper connected on session {} via {}",
        child.session, child.via
    ));
    if !child.note.is_empty() {
        log.line(&format!("desk token: {}", child.note));
    }
    *lock(&pipe_slot) = Some(pipe.duplicate()?);
    // A new helper has not heard about a player that is already attached.
    generation.fetch_add(1, Ordering::SeqCst);
    let reader_pipe = pipe.duplicate()?;
    let reader = thread::Builder::new()
        .name("etherquic-desk-read".into())
        .spawn({
            let stop = Arc::clone(&stop);
            let outbound = Arc::clone(&outbound);
            let log = log.clone();
            let picture = Arc::clone(picture);
            move || reader_loop(reader_pipe, stop, outbound, log, picture)
        })
        .map_err(|err| err.to_string())?;
    let _ = reader.join();
    *lock(&pipe_slot) = None;
    drop(child);
    drop(pipe);
    if stop.load(Ordering::SeqCst) {
        Ok(())
    } else {
        Err("desk helper stopped".into())
    }
}

fn reader_loop(
    pipe: Pipe,
    stop: Arc<AtomicBool>,
    outbound: Arc<Mutex<Option<Outbound>>>,
    log: Log,
    picture: Picture,
) {
    let mut saw_frame = false;
    let mut kept_at: Option<Instant> = None;
    let mut saw_audio = false;
    let mut logged_audio_error = false;
    while !stop.load(Ordering::SeqCst) {
        match pipe.read_message(&stop) {
            Ok(None) => return,
            Ok(Some(Message::Raw(raw))) => {
                let rgb = match Rgb::from_raw(&raw) {
                    Ok(rgb) => rgb,
                    Err(error) => {
                        log.line(&format!("picture: {error}"));
                        continue;
                    }
                };
                if !saw_frame {
                    log.line(&format!("picture {}x{} raw", rgb.width, rgb.height));
                    saw_frame = true;
                }
                // The thumbnail is refreshed every 30 s; one JPEG a second is plenty.
                if kept_at.is_none_or(|at| at.elapsed() >= Duration::from_secs(1)) {
                    if let Ok(jpeg) = crate::proto::encode_jpeg(&rgb, 60) {
                        *lock(&picture) = Some(jpeg);
                    }
                    kept_at = Some(Instant::now());
                }
                if let Some(slot) = video_sender(&outbound) {
                    slot.put(etherquic_media::Frame {
                        width: rgb.width,
                        height: rgb.height,
                        rgb: rgb.rgb,
                    });
                }
            }
            Ok(Some(Message::Jpeg(jpeg))) => {
                // A helper from an older build. The service decodes it so the
                // negotiated encoder still gets raw frames.
                if kept_at.is_none_or(|at| at.elapsed() >= Duration::from_secs(1)) {
                    *lock(&picture) = Some(jpeg.clone());
                    kept_at = Some(Instant::now());
                }
                if let (Some(slot), Ok(frame)) = (
                    video_sender(&outbound),
                    etherquic_media::jpeg::decode(&jpeg),
                ) {
                    slot.put(frame);
                }
            }
            Ok(Some(Message::Pcm(bytes))) => match AudioChunk::decode(&bytes) {
                Ok(chunk) => {
                    if !saw_audio
                        && chunk.sample_rate > 0
                        && chunk.channels > 0
                        && !chunk.pcm.is_empty()
                    {
                        log.line(&format!(
                            "audio {} Hz {} ch pcm {} samples",
                            chunk.sample_rate,
                            chunk.channels,
                            chunk.pcm.len()
                        ));
                        saw_audio = true;
                    }
                    if let Some(tx) = audio_sender(&outbound) {
                        let _ = tx.send(chunk);
                    }
                }
                Err(error) if !logged_audio_error => {
                    log.line(&format!("audio: {error}"));
                    logged_audio_error = true;
                }
                Err(_) => {}
            },
            Ok(Some(Message::Note(text))) => {
                if !text.is_empty() {
                    log.line(&text);
                }
            }
            Ok(Some(Message::Ack(bytes))) => {
                if let Some(tx) = ack_sender(&outbound) {
                    let _ = tx.send(bytes);
                }
            }
            Ok(Some(Message::Cursor(bytes))) => {
                if let Some(tx) = cursor_sender(&outbound) {
                    let _ = tx.send(Some(bytes));
                }
            }
            Ok(Some(Message::Eqin(_) | Message::Wake | Message::Release)) => {}
            Err(error) => {
                log.line(&format!("desk read: {error}"));
                return;
            }
        }
    }
}

/// What to tell the helper so it matches `active`.
///
/// `told` is the generation of the helper that was last told, and what it was
/// told. A helper from a newer generation starts idle.
fn session_message(active: bool, told: Option<(u64, bool)>, generation: u64) -> Option<Message> {
    let known = match told {
        Some((seen, was)) if seen == generation => was,
        _ => false,
    };
    match (known, active) {
        (false, true) => Some(Message::Wake),
        (true, false) => Some(Message::Release),
        _ => None,
    }
}

fn input_loop(
    pipe_slot: Arc<Mutex<Option<Pipe>>>,
    mut input: mpsc::Receiver<Vec<Event>>,
    stop: Arc<AtomicBool>,
    session: Arc<AtomicBool>,
    generation: Arc<AtomicU64>,
) {
    // The only writer on the service side of the pipe, so Wake and Release
    // cannot interleave with an EQIN message.
    let mut told: Option<(u64, bool)> = None;
    while !stop.load(Ordering::SeqCst) {
        let active = session.load(Ordering::SeqCst);
        let current = generation.load(Ordering::SeqCst);
        if let Some(message) = session_message(active, told, current) {
            let pipe = lock(&pipe_slot)
                .as_ref()
                .and_then(|pipe| pipe.duplicate().ok());
            if let Some(pipe) = pipe {
                if pipe.write_message(&message, &stop).is_ok() {
                    told = Some((current, active));
                }
            }
        }
        let events = match input.try_recv() {
            Ok(events) => events,
            Err(mpsc::error::TryRecvError::Empty) => {
                thread::sleep(Duration::from_millis(20));
                continue;
            }
            Err(mpsc::error::TryRecvError::Disconnected) => return,
        };
        if events.is_empty() {
            continue;
        }
        let pipe = lock(&pipe_slot)
            .as_ref()
            .and_then(|pipe| pipe.duplicate().ok());
        let Some(pipe) = pipe else {
            continue;
        };
        let Ok(bytes) = encode(&events) else {
            continue;
        };
        let _ = pipe.write_message(&Message::Eqin(bytes), &stop);
    }
}

fn video_sender(outbound: &Mutex<Option<Outbound>>) -> Option<FrameSlot> {
    lock(outbound).as_ref().map(|out| out.video.clone())
}

fn audio_sender(outbound: &Mutex<Option<Outbound>>) -> Option<broadcast::Sender<AudioChunk>> {
    lock(outbound).as_ref().map(|out| out.audio.clone())
}

fn ack_sender(outbound: &Mutex<Option<Outbound>>) -> Option<broadcast::Sender<Vec<u8>>> {
    lock(outbound).as_ref().map(|out| out.acks.clone())
}

fn cursor_sender(
    outbound: &Mutex<Option<Outbound>>,
) -> Option<tokio::sync::watch::Sender<Option<Vec<u8>>>> {
    lock(outbound).as_ref().map(|out| out.cursor.clone())
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|poison| poison.into_inner())
}

#[cfg(test)]
mod tests {
    use super::session_message;
    use crate::proto::Message;

    #[test]
    fn helper_hears_wake_once_and_release_once() {
        assert_eq!(session_message(false, None, 1), None);
        assert_eq!(session_message(true, None, 1), Some(Message::Wake));
        assert_eq!(session_message(true, Some((1, true)), 1), None);
        assert_eq!(
            session_message(false, Some((1, true)), 1),
            Some(Message::Release)
        );
        assert_eq!(session_message(false, Some((1, false)), 1), None);
    }

    #[test]
    fn a_restarted_helper_is_woken_again_mid_session() {
        assert_eq!(
            session_message(true, Some((1, true)), 2),
            Some(Message::Wake)
        );
        assert_eq!(session_message(false, Some((1, true)), 2), None);
    }
}
