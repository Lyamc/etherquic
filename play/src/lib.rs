//! One play session over etherquic.
//!
//! The host sends the desktop and its audio. The player sends input. All
//! three ride one QUIC connection on service [`SERVICE_ID`], in the same
//! priority order as the rest of the tree: input, then audio, then video.
//!
//! Video frames are chosen by codec identity. The desktop session sends
//! MJPEG. Audio samples are interleaved `i16` PCM. Input is
//! an EQIN document per record, the same bytes as `etherquic-input`.

#![forbid(unsafe_code)]

mod ack;
mod capture;
mod codec;
pub mod hostinfo;
mod outbox;
mod pad;
mod picture;
mod pointer;
mod session;
mod svc;
pub mod view_options;
mod wire;

pub use capture::{desktop_audio, DesktopAudio};
pub use codec::{
    catalog, choose_audio, choose_codec, codec_name, recommend, this_audio, this_computer, AudioId,
    AudioMachine, AudioSkip, CodecId, CodecInfo, CodecSkip, CodecSupport, EndSupport,
    MachineCodecs, PicturePref,
};
pub use pad::{
    gamepad_event, pad_report, set_dpad, set_pad_axis, set_pad_button, xinput_buttons,
    xinput_stick_y, PadAxis, PadButton, PadReport,
};
pub use picture::{
    blit_rgb, decode_jpeg, decode_picture, encode_jpeg, encode_picture, fit_rgba, Picture,
    PictureCodec,
};
pub use pointer::{ei_absolute, place_pointer, wayland_wheel, PointerPlace};
pub use session::{play, resample, serve, AudioChunk};
pub use svc::{drop_jitter_enhancement, SvcLayer};
pub use view_options::{
    live, place, slot_for, snapshot, to_remote, LiveView, PadSlot, Placement, ScaleMode, SizeMatch,
    ViewOptions, PAD_SLOTS,
};
pub use wire::{AUDIO_MAGIC, INPUT_MAGIC, VIDEO_MAGIC};

/// Play session. The same id `etherquic` and `etherquic-video` use.
pub const SERVICE_ID: u16 = 4433;

/// Locally administered address of a host. Matches `etherquic`.
pub const HOST_MAC: [u8; 6] = [0x02, 0x00, 0x00, 0x00, 0x00, 0x01];
/// Locally administered address of a player. Matches `etherquic`.
pub const CLIENT_MAC: [u8; 6] = [0x02, 0x00, 0x00, 0x00, 0x00, 0x02];

/// Widest picture the host encodes unless the caller asks otherwise.
pub const DEFAULT_MAX_WIDTH: u32 = 1280;
/// Pictures per second unless the caller asks otherwise.
pub const DEFAULT_FPS: u32 = 10;

/// What the player should show when the session stops for a reason other than
/// the local Stop button.
///
/// A peer that sends an application close stopped on purpose. A timeout, a
/// stateless reset, or a transport abort is a dropped network or a dead process.
pub fn session_end_status(reason: &str) -> String {
    let lower = reason.to_ascii_lowercase();
    if lower.contains("closed by peer") {
        "The host closed the session.".to_string()
    } else if lower.contains("timed out")
        || lower.contains("reset by peer")
        || lower.contains("aborted by peer")
        || lower.contains("connection lost")
        || lower.contains("unreachable")
        || lower.contains("no route")
        || lower == "session closed"
    {
        "The connection dropped.".to_string()
    } else {
        format!("The session ended: {reason}")
    }
}

#[cfg(feature = "desktop")]
mod client_desk;
#[cfg(feature = "desktop")]
mod host_desk;
#[cfg(feature = "desktop")]
mod pad_inject;
#[cfg(feature = "desktop")]
mod player_pad;
#[cfg(all(feature = "desktop", target_os = "linux"))]
mod seat_ei;
#[cfg(all(feature = "desktop", target_os = "linux"))]
mod seat_wl;

#[cfg(feature = "desktop")]
pub use client_desk::{connect_view, ConnectSession};
#[cfg(feature = "desktop")]
pub use host_desk::{audio_devices, display_hz, host_desktop, HostSession};

/// Where a desktop session reports a line the caller can show.
#[cfg(feature = "desktop")]
pub type Status = std::sync::Arc<dyn Fn(&str) + Send + Sync>;

/// Print session lines. The window passes its own status instead.
#[cfg(feature = "desktop")]
pub fn stderr_status() -> Status {
    std::sync::Arc::new(|text: &str| eprintln!("{text}"))
}

#[cfg(feature = "desktop")]
pub(crate) fn format_mac(mac: [u8; 6]) -> String {
    format!(
        "{:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
        mac[0], mac[1], mac[2], mac[3], mac[4], mac[5]
    )
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{0}")]
    Wire(String),
    #[error("{0}")]
    Picture(String),
    #[error("input: {0}")]
    Input(String),
    #[error("session closed")]
    Closed,
}

#[cfg(test)]
mod tests {
    use super::session_end_status;

    #[test]
    fn a_dead_peer_reads_as_a_dropped_connection() {
        assert_eq!(session_end_status("timed out"), "The connection dropped.");
        assert_eq!(
            session_end_status("reset by peer"),
            "The connection dropped."
        );
        assert_eq!(
            session_end_status("aborted by peer: 0"),
            "The connection dropped."
        );
        assert_eq!(
            session_end_status("session closed"),
            "The connection dropped."
        );
    }

    #[test]
    fn an_application_close_reads_as_the_host_stopping() {
        assert_eq!(
            session_end_status("closed by peer: 0"),
            "The host closed the session."
        );
        assert_eq!(
            session_end_status("wire broke"),
            "The session ended: wire broke"
        );
    }
}
