//! Map Ethernet peers to the `SocketAddr` values Quinn stores for a path.

use std::net::{Ipv6Addr, SocketAddr, SocketAddrV6};

use crate::Mac;

/// Encode a MAC (+ optional service id as UDP port) as an IPv6 link-local address.
///
/// The interface identifier is EUI-64 derived from the MAC. Quinn never hands this
/// address to a real IP stack when the datagram backend is ours.
pub fn peer_to_socket_addr(mac: Mac, service_id: u16) -> SocketAddr {
    let port = if service_id == 0 { 1 } else { service_id };
    SocketAddr::V6(SocketAddrV6::new(mac_to_link_local(mac), port, 0, 0))
}

/// Inverse of [`peer_to_socket_addr`]. Returns `None` if the address is not one we minted.
pub fn socket_addr_to_peer(addr: SocketAddr) -> Option<(Mac, u16)> {
    let v6 = match addr {
        SocketAddr::V6(v6) => v6,
        SocketAddr::V4(_) => return None,
    };
    let mac = link_local_to_mac(*v6.ip())?;
    let service_id = if v6.port() == 1 { 0 } else { v6.port() };
    Some((mac, service_id))
}

fn mac_to_link_local(mac: Mac) -> Ipv6Addr {
    // RFC 4291 EUI-64: flip the U/L bit, insert 0xfffe.
    let mut eui = [0u8; 8];
    eui[0] = mac[0] ^ 0x02;
    eui[1] = mac[1];
    eui[2] = mac[2];
    eui[3] = 0xff;
    eui[4] = 0xfe;
    eui[5] = mac[3];
    eui[6] = mac[4];
    eui[7] = mac[5];
    Ipv6Addr::new(
        0xfe80,
        0,
        0,
        0,
        u16::from_be_bytes([eui[0], eui[1]]),
        u16::from_be_bytes([eui[2], eui[3]]),
        u16::from_be_bytes([eui[4], eui[5]]),
        u16::from_be_bytes([eui[6], eui[7]]),
    )
}

fn link_local_to_mac(ip: Ipv6Addr) -> Option<Mac> {
    let seg = ip.segments();
    if seg[0] != 0xfe80 || seg[1] != 0 || seg[2] != 0 || seg[3] != 0 {
        return None;
    }
    let eui = [
        (seg[4] >> 8) as u8,
        (seg[4] & 0xff) as u8,
        (seg[5] >> 8) as u8,
        (seg[5] & 0xff) as u8,
        (seg[6] >> 8) as u8,
        (seg[6] & 0xff) as u8,
        (seg[7] >> 8) as u8,
        (seg[7] & 0xff) as u8,
    ];
    if eui[3] != 0xff || eui[4] != 0xfe {
        return None;
    }
    Some([eui[0] ^ 0x02, eui[1], eui[2], eui[5], eui[6], eui[7]])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_mac_and_service() {
        let mac = [0x02, 0x00, 0x00, 0x00, 0x00, 0xaa];
        let addr = peer_to_socket_addr(mac, 4433);
        assert_eq!(socket_addr_to_peer(addr), Some((mac, 4433)));
    }

    #[test]
    fn service_zero_uses_port_one() {
        let mac = [0x02, 0x00, 0x00, 0x00, 0x00, 0x01];
        let addr = peer_to_socket_addr(mac, 0);
        assert_eq!(addr.port(), 1);
        assert_eq!(socket_addr_to_peer(addr), Some((mac, 0)));
    }
}
