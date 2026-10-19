//! Host / player model for the etherquic console.
//!
//! With the `ui` feature this crate opens the GPUI Desk. The window binary
//! and the Android NativeActivity both call [`run`]. Without that feature
//! the link is only the QUIC handshake, which is what the tests build.

#![cfg_attr(not(windows), forbid(unsafe_code))]

#[cfg(feature = "ui")]
mod badge;
#[cfg(feature = "ui")]
mod extra_keys;
#[cfg(feature = "ui")]
mod overlay;
#[cfg(feature = "ui")]
mod quality_preview;
#[cfg(feature = "ui")]
mod update;
#[cfg(all(feature = "ui", target_os = "macos"))]
mod child_view;
#[cfg(feature = "ui")]
mod hosts;
mod link;
#[cfg(feature = "ui")]
pub mod macos_service;
#[cfg(feature = "ui")]
mod place;
#[cfg(feature = "ui")]
mod prefs;
#[cfg(feature = "ui")]
mod scan;
pub mod scrollbar;
mod session;
#[cfg(feature = "ui")]
mod settings;
mod share;
mod tabs;
#[cfg(feature = "ui")]
mod taskbar;
mod thumb;
#[cfg(feature = "ui")]
mod tray;
#[cfg(feature = "ui")]
mod view;

#[cfg(feature = "ui")]
pub use hosts::{
    ago, clean_name, hosts_path, load_hosts, matches_filter, store_hosts, HostAddr, HostBook,
    HostEntry, Presence, Roster, RosterRow, Sighting, ThisMachine, RECENT_CAP,
};
#[cfg(feature = "ui")]
pub use link::{last_session, live_view, set_host_audio_device, CodecOffer};
pub use link::{list_nics, Link, NicChoice};
#[cfg(feature = "ui")]
pub use prefs::{
    apply_override, audio_announce, audio_for, audio_hint, audio_match_line, backend_hint,
    backend_label, backend_line, capture_rate, chosen_audio, chosen_video, codec_choice_for,
    default_audio, default_video, fps_headline, listed_top, load_connections, load_preferences,
    local_video_line, mac_key, move_to_front, negotiated_from_status, predicted_pair,
    remote_backend_line, remote_hardware_line, resolve_fps, store_connections, store_preferences,
    swap_earlier, swap_later, top_selection, video_announce, video_hint, video_match_line, Applied,
    ConnectionOverride, ConnectionPrefs, FrameRate, Predicted, Preferences, FPS_CHOICES,
    HARDWARE_FPS, SOFTWARE_FPS,
};
pub use session::{
    active_layer, dial_target, format_mac, lanes, taskbar_for, DialTarget, HexMac, Lane, Layer,
    Role, AUDIO_SERVICE_ID, CLIENT_MAC, HOST_MAC, INPUT_SERVICE_ID, PLAY_SERVICE_ID,
};
#[cfg(feature = "ui")]
pub use settings::{
    backends, defaults_path, load_defaults, load_settings, parse_settings, render_settings,
    settings_path, store_defaults, store_settings, Backend, BackendState, Field, NetworkDriver,
    Part, Section, Settings,
};
pub use share::{qr_grid, this_pc, QrGrid, Share, ShareRow};
pub use tabs::{ConnectionTab, ConnectionTabs};
pub use thumb::{caps_details, caps_summary, decode_thumbnail, Thumb};
#[cfg(feature = "ui")]
pub use view::run;

/// Store the NativeActivity app so GPUI can poll it and open a Vulkan surface.
#[cfg(all(feature = "ui", target_os = "android"))]
pub fn bind_android_app(app: android_activity::AndroidApp) {
    gpui::bind_android_app(app);
}
