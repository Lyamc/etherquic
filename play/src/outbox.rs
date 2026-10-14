//! Player input waiting to be written.
//!
//! Keys leave before pointer, wheel, and gamepad events. Consecutive pointer
//! moves with the same buttons collapse into the latest one, so a mouse wiggle
//! cannot fill the queue and drop a key.

use std::collections::VecDeque;
use std::sync::Mutex;

use etherquic_input::{Body, Event};
use tokio::sync::Notify;

#[derive(Default)]
pub struct Outbox {
    keys: VecDeque<Event>,
    rest: VecDeque<Event>,
}

impl Outbox {
    pub fn push(&mut self, event: Event) {
        if matches!(event.body, Body::Key { .. }) {
            self.keys.push_back(event);
            return;
        }
        if let Some(back) = self.rest.back_mut() {
            if same_motion(back, &event) {
                *back = event;
                return;
            }
        }
        self.rest.push_back(event);
    }

    pub fn pop(&mut self) -> Option<Event> {
        self.keys.pop_front().or_else(|| self.rest.pop_front())
    }
}

fn same_motion(pending: &Event, next: &Event) -> bool {
    match (&pending.body, &next.body) {
        (
            Body::Pointer {
                buttons: left,
                wheel_y: 0,
                wheel_x: 0,
                ..
            },
            Body::Pointer {
                buttons: right,
                wheel_y: 0,
                wheel_x: 0,
                ..
            },
        ) => left == right,
        _ => false,
    }
}

/// Shared queue. `push` wakes the task that owns the QUIC write.
pub struct Gate {
    inner: Mutex<Outbox>,
    notify: Notify,
}

impl Gate {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(Outbox::default()),
            notify: Notify::new(),
        }
    }

    pub fn push(&self, event: Event) {
        self.inner
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .push(event);
        self.notify.notify_one();
    }

    pub fn pop(&self) -> Option<Event> {
        self.inner
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .pop()
    }

    pub async fn wait(&self) {
        self.notify.notified().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use etherquic_input::KeySource;

    fn key(code: u32) -> Event {
        Event {
            time_us: code as u64,
            body: Body::Key {
                down: true,
                source: KeySource::Keysym,
                code,
            },
        }
    }

    fn pointer(x: u16) -> Event {
        Event {
            time_us: 0,
            body: Body::Pointer {
                x,
                y: 0,
                buttons: 0,
                wheel_y: 0,
                wheel_x: 0,
            },
        }
    }

    #[test]
    fn a_key_leaves_ahead_of_pointer_motion() {
        let mut box_ = Outbox::default();
        box_.push(pointer(1));
        box_.push(pointer(2));
        box_.push(pointer(3));
        box_.push(key(0xffeb));
        assert_eq!(box_.pop().unwrap().time_us, 0xffeb);
        let moved = box_.pop().unwrap();
        match moved.body {
            Body::Pointer { x, .. } => assert_eq!(x, 3),
            other => panic!("expected the latest pointer, got {other:?}"),
        }
        assert!(box_.pop().is_none());
    }
}
