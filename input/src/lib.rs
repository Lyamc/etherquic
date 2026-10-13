//! Keyboard, mouse, and gamepad events over etherquic.
//!
//! The bytes on the unidirectional QUIC stream are an EQIN v1 recording.
//! Quinn delivers that stream reliably. [`coalesce`] is the lossy step, and
//! the send tool runs it before the first byte is written: a later absolute
//! pointer replaces the earlier one, deltas saturate-add, and each gamepad
//! index keeps its latest snapshot. Keys are never folded.
//!
//! This crate records and carries events. It does not inject them into the OS.
//!
//! On a shared Quinn connection a higher send-stream priority is transmitted
//! first. Input is [`etherquic::PRIORITY_INPUT`] (20), above audio (10) and
//! video (0). `etherquic-input-send` sets that priority before it writes.

#![forbid(unsafe_code)]

mod error;
mod event;
mod script;

pub use error::Error;
pub use event::{
    coalesce, decode, encode, Axes, Body, Buttons, Event, KeySource, Reader, KIND_DELTA,
    KIND_GAMEPAD, KIND_KEY, KIND_POINTER, KIND_SYNC, MAGIC, PAD_BACK, PAD_DOWN, PAD_EAST,
    PAD_GUIDE, PAD_KNOWN, PAD_LB, PAD_LEFT, PAD_LSTICK, PAD_NORTH, PAD_RB, PAD_RIGHT, PAD_RSTICK,
    PAD_SOUTH, PAD_START, PAD_UP, PAD_WEST, SYNC_CAPS, SYNC_KANA, SYNC_KNOWN, SYNC_NUM,
    SYNC_SCROLL, VERSION,
};
pub use script::{format_event, format_script, parse_script};

/// Service id for the input tools. Video uses 4433 and audio uses 4434.
pub const DEFAULT_SERVICE_ID: u16 = 4435;

/// Read EQIN bytes, or a UTF-8 script when the file does not start with `EQIN`.
pub fn load(bytes: &[u8]) -> Result<Vec<Event>, Error> {
    if bytes.starts_with(MAGIC) {
        decode(bytes)
    } else {
        let text = std::str::from_utf8(bytes)
            .map_err(|_| Error::Input("script file is not utf-8".into()))?;
        parse_script(text)
    }
}

/// Encode events as an EQIN byte stream.
pub fn store(events: &[Event]) -> Result<Vec<u8>, Error> {
    encode(events)
}

pub fn summary(events: &[Event]) -> String {
    let mut keys = 0;
    let mut pointers = 0;
    let mut deltas = 0;
    let mut pads = 0;
    let mut syncs = 0;
    let mut unknown = 0;
    for event in events {
        match event.body {
            Body::Key { .. } => keys += 1,
            Body::Pointer { .. } => pointers += 1,
            Body::Delta { .. } => deltas += 1,
            Body::Gamepad { .. } => pads += 1,
            Body::Sync { .. } => syncs += 1,
            Body::Unknown { .. } => unknown += 1,
        }
    }
    format!(
        "input events={} key={keys} pointer={pointers} delta={deltas} pad={pads} sync={syncs} unknown={unknown}",
        events.len()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn input_priority_outranks_audio_and_video() {
        assert!(etherquic::PRIORITY_INPUT > etherquic::PRIORITY_AUDIO);
        assert!(etherquic::PRIORITY_AUDIO > etherquic::PRIORITY_VIDEO);
        assert_eq!(etherquic::PRIORITY_INPUT, 20);
        assert_eq!(etherquic::PRIORITY_AUDIO, 10);
        assert_eq!(etherquic::PRIORITY_VIDEO, 0);
    }

    #[test]
    fn load_picks_script_or_eqin() {
        let script = b"key down keysym 97\n";
        let events = load(script).unwrap();
        assert_eq!(events.len(), 1);
        let wire = store(&events).unwrap();
        assert!(wire.starts_with(b"EQIN"));
        assert_eq!(load(&wire).unwrap(), events);
        assert!(summary(&events).contains("key=1"));
    }
}
