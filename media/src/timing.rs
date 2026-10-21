//! Host clock records waiting to be paired with the frame or block they describe.
//!
//! The host sends one clock just before a picture or an audio block. The
//! player pushes it here when the record arrives and takes it when that
//! picture or block is ready to play.

use std::collections::VecDeque;
use std::sync::Mutex;
use std::time::{Duration, Instant};

pub struct Stamp {
    pub host: Duration,
    /// When this process received the clock record.
    pub recv: Instant,
}

const CAP: usize = 8;

fn queue(video: bool) -> &'static Mutex<VecDeque<Stamp>> {
    if video {
        static VIDEO: Mutex<VecDeque<Stamp>> = Mutex::new(VecDeque::new());
        &VIDEO
    } else {
        static AUDIO: Mutex<VecDeque<Stamp>> = Mutex::new(VecDeque::new());
        &AUDIO
    }
}

fn push(video: bool, host: Duration) {
    let mut slot = queue(video)
        .lock()
        .unwrap_or_else(|poison| poison.into_inner());
    slot.push_back(Stamp {
        host,
        recv: Instant::now(),
    });
    while slot.len() > CAP {
        slot.pop_front();
    }
}

fn take(video: bool) -> Option<Stamp> {
    queue(video)
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .pop_front()
}

pub fn push_video(host: Duration) {
    push(true, host);
}

pub fn take_video() -> Option<Stamp> {
    take(true)
}

pub fn push_audio(host: Duration) {
    push(false, host);
}

pub fn take_audio() -> Option<Stamp> {
    take(false)
}

/// Drop clocks left by a session that has ended.
pub fn clear() {
    for video in [true, false] {
        queue(video)
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .clear();
    }
}
