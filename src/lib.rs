//! QUIC over a pure-Rust Ethernet datagram backend.
//!
//! Quinn stays unaware of Ethernet. Path addresses are opaque `SocketAddr` values
//! minted from MAC + service id. The [`Fabric`] is an in-memory L2 segment so the
//! demo needs no `AF_PACKET`, no `libc` in this crate, and no `cc` / `ring`.
//!
//! Crypto uses [Lyamc/quinn](https://github.com/Lyamc/quinn) `rustls-pure` with
//! the vendored `rustls-rustcrypto` provider (same path as Lyamc/moq-rs).

#![deny(unsafe_code)]

pub mod addr;
pub mod backend;
pub mod capture;
pub mod cursor;
pub mod discover;
pub mod endpoint;
pub mod fabric;
pub mod frame;
#[cfg(windows)]
pub mod hostinput;
pub mod l2_endpoint;
pub mod look;
#[cfg(windows)]
pub mod ndis_link;
pub mod negotiate;
pub mod pcap_link;
pub mod priority;
pub mod socket;
pub mod thumbnail;
pub mod tls;
pub mod udp_endpoint;

/// 6-byte Ethernet MAC address.
pub type Mac = [u8; 6];

pub use addr::{peer_to_socket_addr, socket_addr_to_peer};
pub use backend::{
    default_backend_order, gpu_compute_present, nvidia_present, offer_from, offer_media,
    pick_backend, probe_backends, pyrowave_hardware_encode, pyrowave_library_present,
    pyrowave_ready, pyrowave_ready_with, selected_backend, widen_process_path, BackendProbe,
    MediaBackend,
};
pub use capture::{
    capture_active, capture_dir, capture_from_env, capture_start, capture_stop, note_app,
    note_ether, CaptureError, CaptureInfo, DIR_RECV, DIR_SEND,
};
pub use endpoint::{client_endpoint, server_endpoint, EndpointError};
pub use fabric::Fabric;
pub use frame::{max_quic_datagram, Frame, DEFAULT_ETHER_TYPE};
pub use l2_endpoint::{
    l2_client_endpoint, l2_preference, l2_server_endpoint, set_l2_preference, L2Net,
};
pub use pcap_link::{parse_mac, ETHERTYPE_QUIC};
pub use priority::{
    AUDIO as PRIORITY_AUDIO, INPUT as PRIORITY_INPUT, TRACE as PRIORITY_TRACE,
    VIDEO as PRIORITY_VIDEO,
};
pub use socket::EtherSocket;
pub use tls::{agreed_alpn, alpn_protocols, install_pure_crypto, negotiates, ALPN_ETHERQUIC};
pub use udp_endpoint::{
    udp_accept, udp_client_endpoint, udp_client_endpoint_on, udp_listeners, udp_port_for,
    udp_server_endpoint, UdpListeners, UDP_PORT_TRIES,
};
