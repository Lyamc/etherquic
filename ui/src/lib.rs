//! Host / player model for the etherquic console.
//!
//! The window lives in the `etherquic` binary. This library is the part
//! that can be tested without a GPU: who is allowed a taskbar button, the
//! peer address field, the text a phone reads from the host QR code, and the
//! L2 session the host or the player opens.
//!
//! With the `ui` feature, Host shares this desktop and Connect opens the
//! remote picture, audio, and input. Without that feature the link is only
//! the QUIC handshake, which is what the tests build.

#![forbid(unsafe_code)]

#[cfg(feature = "ui")]
mod hosts;
mod link;
#[cfg(feature = "ui")]
mod prefs;
pub mod scrollbar;
mod session;
#[cfg(feature = "ui")]
mod settings;
mod share;
mod thumb;

#[cfg(feature = "ui")]
pub use hosts::{
    ago, clean_name, hosts_path, load_hosts, matches_filter, store_hosts, HostAddr, HostBook,
    HostEntry, Presence, Roster, RosterRow, Sighting, RECENT_CAP,
};
pub use link::{list_nics, Link, NicChoice};
#[cfg(feature = "ui")]
pub use link::{live_view, set_host_audio_device};
#[cfg(feature = "ui")]
pub use prefs::{
    apply_override, audio_announce, chosen_audio, chosen_video, move_to_front, audio_for, audio_hint, audio_match_line, backend_hint,
    backend_label, backend_line, capture_rate, default_audio, default_video, fps_headline,
    listed_top, load_connections, load_preferences, local_video_line, mac_key, remote_backend_line,
    remote_hardware_line, resolve_fps, store_connections, store_preferences, swap_earlier,
    swap_later, top_selection, video_announce, video_hint, video_match_line, Applied,
    ConnectionOverride, ConnectionPrefs, FrameRate, Preferences, FPS_CHOICES, SOFTWARE_FPS,
};
pub use session::{
    active_layer, dial_target, format_mac, lanes, taskbar_for, DialTarget, HexMac, Lane, Layer,
    Role, AUDIO_SERVICE_ID, CLIENT_MAC, HOST_MAC, INPUT_SERVICE_ID, PLAY_SERVICE_ID,
};
#[cfg(feature = "ui")]
pub use settings::{
    backends, load_settings, parse_settings, render_settings, settings_path, store_settings,
    Backend, BackendState, NetworkDriver, Part, Section, Settings,
};
pub use share::{qr_grid, this_pc, QrGrid, Share, ShareRow};
pub use thumb::{caps_details, caps_summary, decode_thumbnail, Thumb};
