//! QUIC over a pure-Rust Ethernet datagram backend.
//!
//! Quinn stays unaware of Ethernet. Path addresses are opaque `SocketAddr` values
//! minted from MAC + service id. The [`Fabric`] is an in-memory L2 segment so the
//! demo needs no `AF_PACKET`, no `libc` in this crate, and no `cc` / `ring`.
//!
//! Crypto uses [Lyamc/quinn](https://github.com/Lyamc/quinn) `rustls-pure` with
//! the vendored `rustls-rustcrypto` provider (same path as Lyamc/moq-rs).

#![deny(unsafe_code)]
#![recursion_limit = "256"]

pub mod addr;
pub mod backend;
pub mod capture;
pub mod carrier;
pub mod cursor;
pub mod discover;
pub mod endpoint;
pub mod fabric;
pub mod frame;
pub mod fruc;
#[cfg(windows)]
pub mod hostinput;
pub mod l2_endpoint;
pub mod look;
#[cfg(target_os = "macos")]
mod macos_link;
#[cfg(windows)]
pub mod ndis_link;
pub mod negotiate;
pub mod pcap_link;
pub mod presence;
pub mod priority;
pub mod share;
pub mod socket;
pub mod thumbnail;
pub mod tls;
pub mod udp_endpoint;
pub mod upscale;

/// Release label announced in discovery and shown in the window.
/// This is the Cargo package version (`major.minor.patch`).
pub const RELEASE: &str = env!("CARGO_PKG_VERSION");

static QUIC_BINDS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Counts Quinn endpoints this process has constructed.
pub fn note_quic_bind() {
    QUIC_BINDS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
}

pub fn quic_binds() -> u64 {
    QUIC_BINDS.load(std::sync::atomic::Ordering::Relaxed)
}

/// 6-byte Ethernet MAC address.
pub type Mac = [u8; 6];

pub use addr::{peer_to_socket_addr, socket_addr_to_peer};
pub use backend::{
    default_backend_order, gpu_compute_present, native_library_dir_from_maps, native_library_dirs,
    intel_present, nvidia_present, offer_from, offer_media, pick_backend, probe_backends,
    pyrowave_hardware_encode, pyrowave_library_present, pyrowave_ready, pyrowave_ready_with,
    videotoolbox_present,
    selected_backend, widen_process_path, BackendProbe, MediaBackend,
};
pub use capture::{
    capture_active, capture_dir, capture_from_env, capture_start, capture_stop, note_app,
    note_avtp, note_ether, note_rtp, set_capture_carrier, CaptureError, CaptureInfo, DIR_RECV,
    DIR_SEND,
};
pub use endpoint::{client_endpoint, server_endpoint, EndpointError};
pub use fabric::Fabric;
pub use frame::{max_quic_datagram, Frame, DEFAULT_ETHER_TYPE};
pub use l2_endpoint::{
    l2_client_endpoint, l2_preference, l2_server_endpoint, set_l2_preference, L2Net,
};
pub use pcap_link::{
    iface_kind, l2_unavailable_reason, list_faces, parse_mac, IfaceFace, IfaceKind, ETHERTYPE_QUIC,
};
pub use presence::{
    adjust_path, audio_next_path, audio_open_label, capture_fps_path, host_conf_installed,
    host_conf_path, host_effect,
    load as load_presence, parse as parse_presence, parse_adjust, pick_audio_device, presence_path,
    read_audio_next, read_capture_fps, render as render_presence, render_adjust, stage_audio_device,
    staged_audio_device, store as store_presence, take_adjust, write_audio_next, write_capture_fps,
    HostAdjust, HostEffect, Presence,
};
pub use priority::{
    AUDIO as PRIORITY_AUDIO, INPUT as PRIORITY_INPUT, TRACE as PRIORITY_TRACE,
    VIDEO as PRIORITY_VIDEO,
};
pub use share::{Share, ShareRow};
pub use socket::EtherSocket;
pub use tls::{agreed_alpn, alpn_protocols, install_pure_crypto, negotiates, ALPN_ETHERQUIC};
pub use udp_endpoint::{
    udp_accept, udp_client_endpoint, udp_client_endpoint_on, udp_listeners, udp_port_for,
    udp_server_endpoint, UdpListeners, UDP_PORT_TRIES,
};
