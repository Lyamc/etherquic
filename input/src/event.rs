//! EQIN v1, a self-delimiting little-endian input stream.
//!
//! ```text
//! b"EQIN"  version u8 = 1
//! repeated:
//!   payload_len u16le
//!   time_us     u64le
//!   kind        u8
//!   body        [u8; payload_len - 9]
//! ```
//!
//! `payload_len` counts every byte after itself, including the timestamp and
//! the kind. An unknown kind is kept as raw body bytes so a newer sender can
//! pass through an older receiver.

use crate::error::Error;

pub const MAGIC: &[u8; 4] = b"EQIN";
pub const VERSION: u8 = 1;

pub const KIND_KEY: u8 = 1;
pub const KIND_POINTER: u8 = 2;
pub const KIND_DELTA: u8 = 3;
pub const KIND_GAMEPAD: u8 = 4;
pub const KIND_SYNC: u8 = 5;

/// VNC pointer button mask: bit 0 left, 1 middle, 2 right, 3 x1, 4 x2.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Buttons(pub u8);

impl Buttons {
    pub const LEFT: u8 = 1 << 0;
    pub const MIDDLE: u8 = 1 << 1;
    pub const RIGHT: u8 = 1 << 2;
    pub const X1: u8 = 1 << 3;
    pub const X2: u8 = 1 << 4;
    pub const KNOWN: u8 = Self::LEFT | Self::MIDDLE | Self::RIGHT | Self::X1 | Self::X2;
}

/// Where `Body::Key::code` came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeySource {
    /// USB HID usage: high 16 bits are the usage page, low 16 bits the usage.
    Hid = 0,
    /// RDP scancode set 1. Bit 8 (0x100) marks an extended (E0) key.
    Set1 = 1,
    /// VNC / X11 keysym.
    Keysym = 2,
}

/// Stick and trigger axes. Sticks use the full i16 range (positive X right,
/// positive Y down). Triggers use 0..=32767 pressed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Axes {
    pub lx: i16,
    pub ly: i16,
    pub rx: i16,
    pub ry: i16,
    pub lt: i16,
    pub rt: i16,
}

/// W3C / SDL standard gamepad button order, one bit each.
pub const PAD_SOUTH: u32 = 1 << 0;
pub const PAD_EAST: u32 = 1 << 1;
pub const PAD_WEST: u32 = 1 << 2;
pub const PAD_NORTH: u32 = 1 << 3;
pub const PAD_LB: u32 = 1 << 4;
pub const PAD_RB: u32 = 1 << 5;
pub const PAD_BACK: u32 = 1 << 6;
pub const PAD_START: u32 = 1 << 7;
pub const PAD_GUIDE: u32 = 1 << 8;
pub const PAD_LSTICK: u32 = 1 << 9;
pub const PAD_RSTICK: u32 = 1 << 10;
pub const PAD_UP: u32 = 1 << 11;
pub const PAD_DOWN: u32 = 1 << 12;
pub const PAD_LEFT: u32 = 1 << 13;
pub const PAD_RIGHT: u32 = 1 << 14;
pub const PAD_KNOWN: u32 = (1 << 15) - 1;

pub const SYNC_CAPS: u8 = 1 << 0;
pub const SYNC_NUM: u8 = 1 << 1;
pub const SYNC_SCROLL: u8 = 1 << 2;
pub const SYNC_KANA: u8 = 1 << 3;
pub const SYNC_KNOWN: u8 = SYNC_CAPS | SYNC_NUM | SYNC_SCROLL | SYNC_KANA;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Event {
    /// Microseconds. Zero when the script did not stamp a time.
    pub time_us: u64,
    pub body: Body,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Body {
    Key {
        down: bool,
        source: KeySource,
        code: u32,
    },
    /// Absolute pointer. `x` and `y` are 0..=65535 from the top left.
    /// `wheel_y` positive is away from the user. `wheel_x` positive is right.
    Pointer {
        x: u16,
        y: u16,
        buttons: u8,
        wheel_y: i16,
        wheel_x: i16,
    },
    Delta {
        dx: i16,
        dy: i16,
        buttons: u8,
        wheel_y: i16,
        wheel_x: i16,
    },
    Gamepad {
        index: u8,
        buttons: u32,
        axes: Axes,
    },
    /// Lock-key state. Bits are [`SYNC_CAPS`], [`SYNC_NUM`], [`SYNC_SCROLL`], [`SYNC_KANA`].
    Sync {
        flags: u8,
    },
    Unknown {
        kind: u8,
        body: Vec<u8>,
    },
}

/// Incremental decoder. Feed any cut of the byte stream; call [`Reader::finish`]
/// at the end so a cut-off event is an error instead of a silent drop.
#[derive(Debug, Default)]
pub struct Reader {
    buf: Vec<u8>,
    header_done: bool,
}

impl Reader {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, data: &[u8]) -> Result<Vec<Event>, Error> {
        self.buf.extend_from_slice(data);
        if !self.header_done {
            if self.buf.len() < 5 {
                return Ok(Vec::new());
            }
            if self.buf[..4] != MAGIC[..] {
                return Err(Error::Input("bad magic, expected EQIN".into()));
            }
            let version = self.buf[4];
            if version != VERSION {
                return Err(Error::Input(format!("unsupported EQIN version {version}")));
            }
            self.buf.drain(..5);
            self.header_done = true;
        }

        let mut events = Vec::new();
        loop {
            if self.buf.len() < 2 {
                break;
            }
            let len = u16::from_le_bytes([self.buf[0], self.buf[1]]) as usize;
            if self.buf.len() < 2 + len {
                break;
            }
            let payload = self.buf[2..2 + len].to_vec();
            self.buf.drain(..2 + len);
            events.push(parse_payload(&payload)?);
        }
        Ok(events)
    }

    pub fn finish(self) -> Result<(), Error> {
        if !self.header_done || !self.buf.is_empty() {
            return Err(Error::Input("truncated EQIN stream".into()));
        }
        Ok(())
    }
}

pub fn encode(events: &[Event]) -> Result<Vec<u8>, Error> {
    let mut out = Vec::new();
    out.extend_from_slice(MAGIC);
    out.push(VERSION);
    for event in events {
        push_event(&mut out, event)?;
    }
    Ok(out)
}

pub fn decode(bytes: &[u8]) -> Result<Vec<Event>, Error> {
    let mut reader = Reader::new();
    let events = reader.push(bytes)?;
    reader.finish()?;
    Ok(events)
}

/// Fold events that have not been written yet.
///
/// The QUIC stream itself is reliable. This is the lossy step, and it runs
/// only on bytes that have not been handed to the stream:
/// a later absolute pointer replaces the earlier one, pointer deltas
/// saturate-add, and a gamepad snapshot replaces the earlier snapshot of the
/// same index. The surviving event stays at the later position, so a key
/// that arrived between the two is kept ahead of it. Keys, sync, and unknown
/// kinds are never folded. An absolute pointer is not combined with a delta.
pub fn coalesce(events: &[Event]) -> Vec<Event> {
    let mut out = Vec::with_capacity(events.len());
    for event in events {
        match &event.body {
            Body::Pointer { .. } => {
                if let Some(pos) = out
                    .iter()
                    .rposition(|earlier: &Event| matches!(&earlier.body, Body::Pointer { .. }))
                {
                    out.remove(pos);
                }
                out.push(event.clone());
            }
            Body::Delta {
                dx,
                dy,
                buttons,
                wheel_y,
                wheel_x,
            } => {
                if let Some(pos) = out
                    .iter()
                    .rposition(|earlier: &Event| matches!(&earlier.body, Body::Delta { .. }))
                {
                    let prev = out.remove(pos);
                    let Body::Delta {
                        dx: px,
                        dy: py,
                        wheel_y: pwy,
                        wheel_x: pwx,
                        ..
                    } = prev.body
                    else {
                        out.push(event.clone());
                        continue;
                    };
                    out.push(Event {
                        time_us: event.time_us,
                        body: Body::Delta {
                            dx: px.saturating_add(*dx),
                            dy: py.saturating_add(*dy),
                            buttons: *buttons,
                            wheel_y: pwy.saturating_add(*wheel_y),
                            wheel_x: pwx.saturating_add(*wheel_x),
                        },
                    });
                } else {
                    out.push(event.clone());
                }
            }
            Body::Gamepad { index, .. } => {
                if let Some(pos) = out.iter().rposition(|earlier: &Event| match &earlier.body {
                    Body::Gamepad { index: got, .. } => got == index,
                    _ => false,
                }) {
                    out.remove(pos);
                }
                out.push(event.clone());
            }
            _ => out.push(event.clone()),
        }
    }
    out
}

fn push_event(out: &mut Vec<u8>, event: &Event) -> Result<(), Error> {
    let mut payload = Vec::with_capacity(24);
    payload.extend_from_slice(&event.time_us.to_le_bytes());
    let (kind, body) = encode_body(&event.body)?;
    payload.push(kind);
    payload.extend_from_slice(&body);
    let len =
        u16::try_from(payload.len()).map_err(|_| Error::Input("event longer than u16".into()))?;
    out.extend_from_slice(&len.to_le_bytes());
    out.extend_from_slice(&payload);
    Ok(())
}

fn encode_body(body: &Body) -> Result<(u8, Vec<u8>), Error> {
    let mut raw = Vec::new();
    let kind = match body {
        Body::Key { down, source, code } => {
            raw.push(u8::from(*down));
            raw.push(*source as u8);
            raw.extend_from_slice(&code.to_le_bytes());
            KIND_KEY
        }
        Body::Pointer {
            x,
            y,
            buttons,
            wheel_y,
            wheel_x,
        } => {
            raw.extend_from_slice(&x.to_le_bytes());
            raw.extend_from_slice(&y.to_le_bytes());
            raw.push(*buttons);
            raw.extend_from_slice(&wheel_y.to_le_bytes());
            raw.extend_from_slice(&wheel_x.to_le_bytes());
            KIND_POINTER
        }
        Body::Delta {
            dx,
            dy,
            buttons,
            wheel_y,
            wheel_x,
        } => {
            raw.extend_from_slice(&dx.to_le_bytes());
            raw.extend_from_slice(&dy.to_le_bytes());
            raw.push(*buttons);
            raw.extend_from_slice(&wheel_y.to_le_bytes());
            raw.extend_from_slice(&wheel_x.to_le_bytes());
            KIND_DELTA
        }
        Body::Gamepad {
            index,
            buttons,
            axes,
        } => {
            raw.push(*index);
            raw.extend_from_slice(&buttons.to_le_bytes());
            for axis in [axes.lx, axes.ly, axes.rx, axes.ry, axes.lt, axes.rt] {
                raw.extend_from_slice(&axis.to_le_bytes());
            }
            KIND_GAMEPAD
        }
        Body::Sync { flags } => {
            raw.push(*flags);
            KIND_SYNC
        }
        Body::Unknown { kind, body } => {
            if matches!(
                *kind,
                KIND_KEY | KIND_POINTER | KIND_DELTA | KIND_GAMEPAD | KIND_SYNC
            ) {
                return Err(Error::Input(format!(
                    "kind {kind} is known and cannot be written as unknown"
                )));
            }
            raw.extend_from_slice(body);
            *kind
        }
    };
    Ok((kind, raw))
}

fn parse_payload(payload: &[u8]) -> Result<Event, Error> {
    if payload.len() < 9 {
        return Err(Error::Input("event shorter than timestamp and kind".into()));
    }
    let mut time = [0u8; 8];
    time.copy_from_slice(&payload[..8]);
    let time_us = u64::from_le_bytes(time);
    let kind = payload[8];
    let body = decode_body(kind, &payload[9..])?;
    Ok(Event { time_us, body })
}

fn decode_body(kind: u8, body: &[u8]) -> Result<Body, Error> {
    match kind {
        KIND_KEY => {
            if body.len() != 6 {
                return Err(Error::Input(format!(
                    "key body is {} bytes, expected 6",
                    body.len()
                )));
            }
            let down = match body[0] {
                0 => false,
                1 => true,
                other => {
                    return Err(Error::Input(format!("key down flag {other} is not 0 or 1")));
                }
            };
            let source = match body[1] {
                0 => KeySource::Hid,
                1 => KeySource::Set1,
                2 => KeySource::Keysym,
                other => {
                    return Err(Error::Input(format!("unknown key source {other}")));
                }
            };
            let mut code = [0u8; 4];
            code.copy_from_slice(&body[2..6]);
            Ok(Body::Key {
                down,
                source,
                code: u32::from_le_bytes(code),
            })
        }
        KIND_POINTER => {
            let (x, y, buttons, wheel_y, wheel_x) = pointer_fields(body, "pointer")?;
            Ok(Body::Pointer {
                x,
                y,
                buttons,
                wheel_y,
                wheel_x,
            })
        }
        KIND_DELTA => {
            if body.len() != 9 {
                return Err(Error::Input(format!(
                    "delta body is {} bytes, expected 9",
                    body.len()
                )));
            }
            Ok(Body::Delta {
                dx: i16_at(body, 0),
                dy: i16_at(body, 2),
                buttons: body[4],
                wheel_y: i16_at(body, 5),
                wheel_x: i16_at(body, 7),
            })
        }
        KIND_GAMEPAD => {
            if body.len() != 17 {
                return Err(Error::Input(format!(
                    "gamepad body is {} bytes, expected 17",
                    body.len()
                )));
            }
            let mut buttons = [0u8; 4];
            buttons.copy_from_slice(&body[1..5]);
            Ok(Body::Gamepad {
                index: body[0],
                buttons: u32::from_le_bytes(buttons),
                axes: Axes {
                    lx: i16_at(body, 5),
                    ly: i16_at(body, 7),
                    rx: i16_at(body, 9),
                    ry: i16_at(body, 11),
                    lt: i16_at(body, 13),
                    rt: i16_at(body, 15),
                },
            })
        }
        KIND_SYNC => {
            if body.len() != 1 {
                return Err(Error::Input(format!(
                    "sync body is {} bytes, expected 1",
                    body.len()
                )));
            }
            Ok(Body::Sync { flags: body[0] })
        }
        kind => Ok(Body::Unknown {
            kind,
            body: body.to_vec(),
        }),
    }
}

fn pointer_fields(body: &[u8], name: &str) -> Result<(u16, u16, u8, i16, i16), Error> {
    if body.len() != 9 {
        return Err(Error::Input(format!(
            "{name} body is {} bytes, expected 9",
            body.len()
        )));
    }
    Ok((
        u16::from_le_bytes([body[0], body[1]]),
        u16::from_le_bytes([body[2], body[3]]),
        body[4],
        i16_at(body, 5),
        i16_at(body, 7),
    ))
}

fn i16_at(body: &[u8], at: usize) -> i16 {
    i16::from_le_bytes([body[at], body[at + 1]])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Vec<Event> {
        vec![
            Event {
                time_us: 10,
                body: Body::Key {
                    down: true,
                    source: KeySource::Hid,
                    code: 0x0007_0004,
                },
            },
            Event {
                time_us: 11,
                body: Body::Key {
                    down: false,
                    source: KeySource::Set1,
                    code: 0x11D,
                },
            },
            Event {
                time_us: 12,
                body: Body::Key {
                    down: true,
                    source: KeySource::Keysym,
                    code: 0x61,
                },
            },
            Event {
                time_us: 20,
                body: Body::Pointer {
                    x: 0,
                    y: 65535,
                    buttons: Buttons::LEFT | Buttons::RIGHT,
                    wheel_y: 1,
                    wheel_x: -2,
                },
            },
            Event {
                time_us: 30,
                body: Body::Delta {
                    dx: -4,
                    dy: 5,
                    buttons: Buttons::MIDDLE,
                    wheel_y: 0,
                    wheel_x: 1,
                },
            },
            Event {
                time_us: 40,
                body: Body::Gamepad {
                    index: 1,
                    buttons: PAD_SOUTH | PAD_UP,
                    axes: Axes {
                        lx: -100,
                        ly: 200,
                        rx: 0,
                        ry: -1,
                        lt: 32767,
                        rt: 0,
                    },
                },
            },
            Event {
                time_us: 50,
                body: Body::Sync {
                    flags: SYNC_CAPS | SYNC_KANA,
                },
            },
            Event {
                time_us: 60,
                body: Body::Unknown {
                    kind: 99,
                    body: vec![1, 2, 3, 4],
                },
            },
        ]
    }

    #[test]
    fn each_kind_roundtrips() {
        let events = sample();
        let bytes = encode(&events).unwrap();
        assert_eq!(decode(&bytes).unwrap(), events);
    }

    #[test]
    fn key_layout_fixture() {
        let bytes = [
            b'E', b'Q', b'I', b'N', 1, // header
            15, 0, // payload length
            1, 0, 0, 0, 0, 0, 0, 0, // time = 1
            1, // kind key
            1, 0, // down, hid
            4, 0, 0, 0, // code = 4
        ];
        let events = decode(&bytes).unwrap();
        assert_eq!(
            events,
            vec![Event {
                time_us: 1,
                body: Body::Key {
                    down: true,
                    source: KeySource::Hid,
                    code: 4,
                },
            }]
        );
        assert_eq!(encode(&events).unwrap(), bytes);
    }

    #[test]
    fn unknown_kind_is_preserved() {
        let event = Event {
            time_us: 7,
            body: Body::Unknown {
                kind: 0,
                body: vec![],
            },
        };
        let back = decode(&encode(&[event.clone()]).unwrap()).unwrap();
        assert_eq!(back, vec![event]);
    }

    #[test]
    fn known_kind_with_the_wrong_size_is_an_error() {
        let mut bytes = encode(&[]).unwrap();
        let mut payload = vec![0u8; 8];
        payload.push(KIND_KEY);
        payload.extend_from_slice(&[1, 0, 4]);
        bytes.extend_from_slice(&(payload.len() as u16).to_le_bytes());
        bytes.extend_from_slice(&payload);
        assert!(decode(&bytes).is_err());
    }

    #[test]
    fn truncated_streams_fail() {
        let bytes = encode(&sample()).unwrap();
        assert!(decode(&[]).unwrap_err().to_string().contains("truncated"));
        assert!(decode(&bytes[..4])
            .unwrap_err()
            .to_string()
            .contains("truncated"));
        assert!(decode(&bytes[..bytes.len() - 1])
            .unwrap_err()
            .to_string()
            .contains("truncated"));
        let mut short = encode(&[]).unwrap();
        short.extend_from_slice(&4u16.to_le_bytes());
        short.extend_from_slice(&[1, 2, 3, 4]);
        assert!(decode(&short).unwrap_err().to_string().contains("shorter"));
    }

    #[test]
    fn bad_magic_and_version() {
        assert!(decode(b"EQIX\x01")
            .unwrap_err()
            .to_string()
            .contains("magic"));
        assert!(decode(b"EQIN\x02")
            .unwrap_err()
            .to_string()
            .contains("version"));
    }

    #[test]
    fn reader_split_at_every_byte() {
        let events = sample();
        let bytes = encode(&events).unwrap();
        for cut in 0..=bytes.len() {
            let mut reader = Reader::new();
            let mut got = reader.push(&bytes[..cut]).unwrap();
            got.extend(reader.push(&bytes[cut..]).unwrap());
            reader.finish().unwrap();
            assert_eq!(got, events, "cut {cut}");
        }
        let mut reader = Reader::new();
        let mut got = Vec::new();
        for byte in &bytes {
            got.extend(reader.push(&[*byte]).unwrap());
        }
        reader.finish().unwrap();
        assert_eq!(got, events);
    }

    #[test]
    fn header_only_is_an_empty_stream() {
        assert_eq!(decode(&encode(&[]).unwrap()).unwrap(), Vec::<Event>::new());
    }

    #[test]
    fn coalesce_folds_state_and_keeps_keys() {
        let key = Event {
            time_us: 2,
            body: Body::Key {
                down: true,
                source: KeySource::Keysym,
                code: 0x61,
            },
        };
        let pointer = |time, x, wheel| Event {
            time_us: time,
            body: Body::Pointer {
                x,
                y: 1,
                buttons: Buttons::LEFT,
                wheel_y: wheel,
                wheel_x: 0,
            },
        };
        let folded = coalesce(&[pointer(1, 1, 1), key.clone(), pointer(3, 9, 4)]);
        assert_eq!(folded, vec![key.clone(), pointer(3, 9, 4)]);

        let delta = |time, dx, buttons, wheel| Event {
            time_us: time,
            body: Body::Delta {
                dx,
                dy: 1,
                buttons,
                wheel_y: wheel,
                wheel_x: -1,
            },
        };
        let folded = coalesce(&[
            delta(1, i16::MAX, Buttons::LEFT, 2),
            delta(2, 1, 0, i16::MAX),
        ]);
        assert_eq!(
            folded,
            vec![Event {
                time_us: 2,
                body: Body::Delta {
                    dx: i16::MAX,
                    dy: 2,
                    buttons: 0,
                    wheel_y: i16::MAX,
                    wheel_x: -2,
                },
            }]
        );
        let folded = coalesce(&[delta(1, i16::MIN, 0, 0), delta(2, -1, 0, 0)]);
        match &folded[0].body {
            Body::Delta { dx, .. } => assert_eq!(*dx, i16::MIN),
            other => panic!("expected delta, got {other:?}"),
        }

        let pad = |index, buttons, time| Event {
            time_us: time,
            body: Body::Gamepad {
                index,
                buttons,
                axes: Axes::default(),
            },
        };
        let folded = coalesce(&[pad(0, 1, 1), pad(1, 2, 2), pad(0, 4, 3)]);
        assert_eq!(folded, vec![pad(1, 2, 2), pad(0, 4, 3)]);

        let repeated = coalesce(&[key.clone(), key.clone()]);
        assert_eq!(repeated.len(), 2);

        let pointer_then_delta = coalesce(&[pointer(1, 1, 0), delta(2, 3, 0, 0)]);
        assert_eq!(pointer_then_delta.len(), 2);
    }
}
