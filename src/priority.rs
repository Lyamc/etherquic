//! Quinn send-stream priorities for the media tools.
//!
//! A higher value is transmitted first. Input events outrank audio, and
//! audio outranks video. The gaps leave room for a later level (a mic
//! stream, a video retransmission) without reordering these three.

/// Keyboard, mouse, and gamepad events.
pub const INPUT: i32 = 20;
/// Opus audio.
pub const AUDIO: i32 = 10;
/// Video file or frames.
pub const VIDEO: i32 = 0;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn input_outranks_audio_outranks_video() {
        assert!(INPUT > AUDIO);
        assert!(AUDIO > VIDEO);
    }
}
