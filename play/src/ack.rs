//! Confirmation that the host applied one stamped input event.
//!
//! The player sets [`Event::time_us`] when tracing is on. The host echoes that
//! stamp on an `EACK` stream after it has applied the event. The player
//! subtracts the send time. A zero stamp is not confirmed, so an old host and
//! a session with tracing off stay quiet.

use std::time::Duration;

use etherquic_input::{Body, Event};

use crate::Error;

pub const ACK_MAGIC: &[u8; 4] = b"EACK";

pub const ACK_KEY: u8 = 1;
pub const ACK_POINTER: u8 = 2;
pub const ACK_OTHER: u8 = 3;

pub const ACK_APPLIED: u8 = 0;
pub const ACK_IGNORED: u8 = 1;
pub const ACK_FAILED: u8 = 2;

/// Fifteen bytes: stamp, kind, code, result, detail.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InputAck {
    pub time_us: u64,
    pub kind: u8,
    pub code: u32,
    pub result: u8,
    /// Key down is `1`, key up is `0`. Other events leave this at `0`.
    pub detail: u8,
}

impl InputAck {
    pub fn encode(self) -> [u8; 15] {
        let mut out = [0u8; 15];
        out[..8].copy_from_slice(&self.time_us.to_le_bytes());
        out[8] = self.kind;
        out[9..13].copy_from_slice(&self.code.to_le_bytes());
        out[13] = self.result;
        out[14] = self.detail;
        out
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, Error> {
        if bytes.len() != 15 {
            return Err(Error::Wire(format!(
                "input confirmation is {} bytes",
                bytes.len()
            )));
        }
        let mut time = [0u8; 8];
        time.copy_from_slice(&bytes[..8]);
        let mut code = [0u8; 4];
        code.copy_from_slice(&bytes[9..13]);
        Ok(Self {
            time_us: u64::from_le_bytes(time),
            kind: bytes[8],
            code: u32::from_le_bytes(code),
            result: bytes[13],
            detail: bytes[14],
        })
    }

    /// `None` when the player did not ask for a confirmation.
    pub fn confirm(event: &Event, outcome: &Result<bool, String>) -> Option<Self> {
        if event.time_us == 0 {
            return None;
        }
        let (kind, code, detail) = match &event.body {
            Body::Key { down, code, .. } => (ACK_KEY, *code, u8::from(*down)),
            Body::Pointer { .. } | Body::Delta { .. } => (ACK_POINTER, 0, 0),
            _ => (ACK_OTHER, 0, 0),
        };
        let result = match outcome {
            Ok(true) => ACK_APPLIED,
            Ok(false) => ACK_IGNORED,
            Err(_) => ACK_FAILED,
        };
        Some(Self {
            time_us: event.time_us,
            kind,
            code,
            result,
            detail,
        })
    }

    pub fn summary(self, elapsed: Option<Duration>) -> String {
        let tail = match elapsed {
            Some(elapsed) => format!(" in {} ms", elapsed.as_millis()),
            None => String::new(),
        };
        let word = match self.result {
            ACK_APPLIED => "applied",
            ACK_IGNORED => "ignored",
            ACK_FAILED => "failed",
            _ => "confirmed",
        };
        match self.kind {
            ACK_KEY => {
                let state = if self.detail == 1 { "down" } else { "up" };
                format!("Input: key {state} {:#x} {word}{tail}", self.code)
            }
            ACK_POINTER => format!("Input: pointer {word}{tail}"),
            _ => format!("Input: event {word}{tail}"),
        }
    }
}

/// Which events get a stamp while tracing is on.
///
/// Keys and clicks are always stamped. Pointer motion is stamped at most
/// once per 100 ms so a moving mouse does not crowd out the key result.
#[derive(Clone, Debug)]
pub struct TraceClock {
    seq: u64,
    buttons: u8,
    last_move: Option<Duration>,
}

impl Default for TraceClock {
    fn default() -> Self {
        Self {
            seq: 0,
            buttons: 0,
            last_move: None,
        }
    }
}

impl TraceClock {
    pub fn stamp(&mut self, now: Duration, event: &mut Event) -> bool {
        let want = match &event.body {
            Body::Key { .. } => true,
            Body::Pointer {
                buttons,
                wheel_y,
                wheel_x,
                ..
            } => {
                let changed = *buttons != self.buttons || *wheel_y != 0 || *wheel_x != 0;
                let due = match self.last_move {
                    Some(last) => now.saturating_sub(last) >= Duration::from_millis(100),
                    None => true,
                };
                if changed {
                    self.buttons = *buttons;
                } else if due {
                    self.last_move = Some(now);
                }
                changed || due
            }
            _ => false,
        };
        if !want {
            event.time_us = 0;
            return false;
        }
        self.seq = self.seq.wrapping_add(1).max(1);
        event.time_us = self.seq;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use etherquic_input::{Buttons, KeySource};

    fn key(code: u32) -> Event {
        Event {
            time_us: 0,
            body: Body::Key {
                down: true,
                source: KeySource::Keysym,
                code,
            },
        }
    }

    fn pointer(buttons: u8) -> Event {
        Event {
            time_us: 0,
            body: Body::Pointer {
                x: 1,
                y: 2,
                buttons,
                wheel_y: 0,
                wheel_x: 0,
            },
        }
    }

    #[test]
    fn confirmation_round_trips() {
        let event = key(0xffeb);
        let ack = InputAck::confirm(&event, &Ok(true));
        assert!(ack.is_none());
        let mut stamped = event;
        stamped.time_us = 7;
        let ack = InputAck::confirm(&stamped, &Ok(true)).unwrap();
        assert_eq!(InputAck::decode(&ack.encode()).unwrap(), ack);
        assert_eq!(
            ack.summary(Some(Duration::from_millis(18))),
            "Input: key down 0xffeb applied in 18 ms"
        );
        let ignored = InputAck::confirm(&stamped, &Ok(false)).unwrap();
        assert_eq!(ignored.result, ACK_IGNORED);
        let failed = InputAck::confirm(&stamped, &Err("SendInput".into())).unwrap();
        assert_eq!(failed.result, ACK_FAILED);
    }

    #[test]
    fn keys_are_stamped_and_pointer_motion_is_sampled() {
        let mut clock = TraceClock::default();
        let mut press = key(0xffeb);
        assert!(clock.stamp(Duration::from_millis(0), &mut press));
        assert_eq!(press.time_us, 1);

        let mut first = pointer(0);
        let mut soon = pointer(0);
        let mut later = pointer(0);
        let mut click = pointer(Buttons::LEFT);
        assert!(clock.stamp(Duration::from_millis(0), &mut first));
        assert!(!clock.stamp(Duration::from_millis(50), &mut soon));
        assert_eq!(soon.time_us, 0);
        assert!(clock.stamp(Duration::from_millis(150), &mut later));
        assert!(clock.stamp(Duration::from_millis(160), &mut click));
        assert_ne!(first.time_us, 0);
        assert_ne!(later.time_us, first.time_us);
        assert_ne!(click.time_us, 0);
    }
}
