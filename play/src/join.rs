//! Join a host from the share-card text a phone scans.
//!
//! Android has no EtherType socket. The player dials the UDP port on the
//! card, then runs the same [`crate::play_session`] a desktop Connect uses.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use etherquic::share::Share;
use etherquic::{install_pure_crypto, udp_client_endpoint_on};
use etherquic_input::Event;
use tokio::sync::mpsc;

use crate::pipes::Pipes;
use crate::session::{play_session, AudioChunk, PlayerMedia};
use crate::Error;

/// Parse the host-card text a camera app shows after a QR scan.
pub fn parse_share(text: &str) -> Result<Share, Error> {
    Share::parse(text).ok_or_else(|| Error::Wire("share card text is not etherquic".into()))
}

/// UDP address a phone dials from a parsed card.
pub fn udp_peer(share: &Share) -> Result<SocketAddr, Error> {
    share
        .udp_peer()
        .ok_or_else(|| Error::Wire("share card has no UDP address".into()))
}

/// Dial `peer` over UDP and run a play session until it ends or `stop` is set.
pub async fn join_udp(
    peer: SocketAddr,
    media: PlayerMedia,
    audio: mpsc::Sender<AudioChunk>,
    input: mpsc::Receiver<Vec<Event>>,
    stop: Arc<AtomicBool>,
) -> Result<(), Error> {
    install_pure_crypto();
    let bind = if peer.is_ipv4() {
        SocketAddr::from(([0, 0, 0, 0], 0))
    } else {
        SocketAddr::from((std::net::Ipv6Addr::UNSPECIFIED, 0))
    };
    let endpoint = udp_client_endpoint_on(bind).map_err(|error| Error::Wire(error.to_string()))?;
    let connecting = endpoint
        .connect(peer, "localhost")
        .map_err(|error| Error::Wire(error.to_string()))?;
    let conn = tokio::select! {
        result = tokio::time::timeout(Duration::from_secs(10), connecting) => {
            match result {
                Ok(Ok(conn)) => conn,
                Ok(Err(error)) => return Err(Error::Wire(error.to_string())),
                Err(_) => return Err(Error::Wire("connect timed out".into())),
            }
        }
        _ = until_stop(&stop) => return Err(Error::Closed),
    };
    let session = play_session(conn, media, audio, input, None, Pipes::idle(), None);
    tokio::select! {
        result = session => result,
        _ = until_stop(&stop) => Err(Error::Closed),
    }
}

async fn until_stop(stop: &AtomicBool) {
    while !stop.load(Ordering::SeqCst) {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;

    #[test]
    fn share_text_names_the_udp_peer() {
        let text = "etherquic\nmac 02:00:00:00:00:01\nservice 4433\nport 4434\nip 192.168.0.24\n";
        let share = parse_share(text).expect("share");
        assert_eq!(
            udp_peer(&share).expect("udp"),
            SocketAddr::from((Ipv4Addr::new(192, 168, 0, 24), 4434))
        );
    }

    #[test]
    fn foreign_text_is_rejected() {
        assert!(parse_share("hello\nport 1\n").is_err());
        let share = parse_share("etherquic\nmac 02:00:00:00:00:01\nservice 4433\nport 4434\n")
            .expect("share");
        assert!(udp_peer(&share).is_err());
    }
}
