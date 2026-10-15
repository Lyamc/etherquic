//! Keep the desktop helper alive and feed one play session from its pipe.
//!
//! The helper stays up across players. A new picture replaces the previous
//! one. Audio is dropped when the player is behind. Input is written as EQIN
//! and injected on the console desktop.

#![forbid(unsafe_code)]

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use etherquic_input::{encode, Event};
use tokio::sync::{mpsc, watch};

use crate::link::{self, Pipe};
use crate::proto::{AudioChunk, Message};
use crate::run::Log;

struct Outbound {
    video: watch::Sender<Vec<u8>>,
    audio: mpsc::Sender<AudioChunk>,
    acks: mpsc::Sender<Vec<u8>>,
}

pub struct Desk {
    stop: Arc<AtomicBool>,
    outbound: Arc<Mutex<Option<Outbound>>>,
    input: mpsc::Sender<Vec<Event>>,
    supervisor: Option<JoinHandle<()>>,
    writer: Option<JoinHandle<()>>,
}

impl Desk {
    pub fn spawn(log: &Log) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let outbound = Arc::new(Mutex::new(None));
        let pipe_slot = Arc::new(Mutex::new(None));
        let (input_tx, input_rx) = mpsc::channel(64);
        let writer_stop = Arc::clone(&stop);
        let writer_slot = Arc::clone(&pipe_slot);
        let writer = thread::Builder::new()
            .name("etherquic-input".into())
            .spawn(move || input_loop(writer_slot, input_rx, writer_stop))
            .ok();
        let supervisor = thread::Builder::new()
            .name("etherquic-desk".into())
            .spawn({
                let stop = Arc::clone(&stop);
                let pipe_slot = Arc::clone(&pipe_slot);
                let outbound = Arc::clone(&outbound);
                let log = log.clone();
                move || supervise(stop, pipe_slot, outbound, log)
            })
            .ok();
        if writer.is_none() || supervisor.is_none() {
            log.line("desk thread failed to start");
        }
        Self {
            stop,
            outbound,
            input: input_tx,
            supervisor,
            writer,
        }
    }

    pub fn input(&self) -> mpsc::Sender<Vec<Event>> {
        self.input.clone()
    }

    pub fn attach(
        &self,
        video: watch::Sender<Vec<u8>>,
        audio: mpsc::Sender<AudioChunk>,
        acks: mpsc::Sender<Vec<u8>>,
    ) {
        *lock(&self.outbound) = Some(Outbound { video, audio, acks });
    }

    pub fn detach(&self) {
        *lock(&self.outbound) = None;
    }

    pub async fn serve_player(&self, conn: quinn::Connection) -> Result<(), String> {
        let (video_tx, video_rx) = watch::channel(Vec::new());
        let (audio_tx, audio_rx) = mpsc::channel(8);
        let (ack_tx, ack_rx) = mpsc::channel(64);
        self.attach(video_tx, audio_tx, ack_tx);
        let _guard = Detach(self);
        crate::media::serve(conn, video_rx, audio_rx, self.input(), ack_rx).await
    }
}

struct Detach<'a>(&'a Desk);

impl Drop for Detach<'_> {
    fn drop(&mut self) {
        self.0.detach();
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
    log: Log,
) {
    let mut last_error = String::new();
    let mut logged_at = Instant::now() - Duration::from_secs(60);
    while !stop.load(Ordering::SeqCst) {
        match run_once(
            Arc::clone(&stop),
            Arc::clone(&pipe_slot),
            Arc::clone(&outbound),
            &log,
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
    log: &Log,
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
    let reader_pipe = pipe.duplicate()?;
    let reader = thread::Builder::new()
        .name("etherquic-desk-read".into())
        .spawn({
            let stop = Arc::clone(&stop);
            let outbound = Arc::clone(&outbound);
            let log = log.clone();
            move || reader_loop(reader_pipe, stop, outbound, log)
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
) {
    let mut saw_frame = false;
    let mut saw_audio = false;
    let mut logged_audio_error = false;
    while !stop.load(Ordering::SeqCst) {
        match pipe.read_message(&stop) {
            Ok(None) => return,
            Ok(Some(Message::Jpeg(jpeg))) => {
                if !saw_frame && !jpeg.is_empty() {
                    log.line(&format!("picture {} bytes", jpeg.len()));
                    saw_frame = true;
                }
                if let Some(tx) = video_sender(&outbound) {
                    let _ = tx.send(jpeg);
                }
            }
            Ok(Some(Message::Pcm(bytes))) => match AudioChunk::decode(&bytes) {
                Ok(chunk) => {
                    if !saw_audio && chunk.sample_rate > 0 && chunk.channels > 0 && !chunk.pcm.is_empty()
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
                        let _ = tx.try_send(chunk);
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
                    let _ = tx.try_send(bytes);
                }
            }
            Ok(Some(Message::Eqin(_))) => {}
            Err(error) => {
                log.line(&format!("desk read: {error}"));
                return;
            }
        }
    }
}

fn input_loop(pipe_slot: Arc<Mutex<Option<Pipe>>>, mut input: mpsc::Receiver<Vec<Event>>, stop: Arc<AtomicBool>) {
    while !stop.load(Ordering::SeqCst) {
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
        let pipe = lock(&pipe_slot).as_ref().and_then(|pipe| pipe.duplicate().ok());
        let Some(pipe) = pipe else {
            continue;
        };
        let Ok(bytes) = encode(&events) else {
            continue;
        };
        let _ = pipe.write_message(&Message::Eqin(bytes), &stop);
    }
}

fn video_sender(outbound: &Mutex<Option<Outbound>>) -> Option<watch::Sender<Vec<u8>>> {
    lock(outbound).as_ref().map(|out| out.video.clone())
}

fn audio_sender(outbound: &Mutex<Option<Outbound>>) -> Option<mpsc::Sender<AudioChunk>> {
    lock(outbound).as_ref().map(|out| out.audio.clone())
}

fn ack_sender(outbound: &Mutex<Option<Outbound>>) -> Option<mpsc::Sender<Vec<u8>>> {
    lock(outbound).as_ref().map(|out| out.acks.clone())
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|poison| poison.into_inner())
}
