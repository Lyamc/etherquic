//! Host / player model for the etherquic console.
//!
//! The window lives in the `etherquic-ui` binary. This library is the part
//! that can be tested without a GPU: who is allowed a taskbar button, the
//! peer address field, and the L2 session the host or the player opens.

#![forbid(unsafe_code)]

mod link;
mod session;

pub use link::{list_nics, Link, NicChoice};
pub use session::{
    format_mac, lanes, taskbar_for, HexMac, Lane, Role, AUDIO_SERVICE_ID, CLIENT_MAC, HOST_MAC,
    INPUT_SERVICE_ID, PLAY_SERVICE_ID,
};
