//! Quinn send-stream priorities for the media tools.
//!
//! A higher value is transmitted first. Input events outrank audio, and
//! audio outranks video. The gaps leave room for a later level (a mic
//! stream, a video retransmission) without reordering these three.

/// Host confirmation of one input event. Above input so the report is not
/// queued behind the event it is timing, and above audio and video.
pub const TRACE: i32 = 30;
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
        assert!(TRACE > INPUT);
        assert!(INPUT > AUDIO);
        assert!(AUDIO > VIDEO);
    }
}
