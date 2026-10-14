# QUIC over Ethernet

Quinn works on Ethernet when the backend looks like UDP and hides the rest.

Each send and receive is one datagram. The peer address is stable for the life of the path. The backend tells Quinn the maximum datagram size and refuses anything larger before it hits the wire. A 1200-byte Initial fits in one frame.

Packet protection, connection IDs, loss recovery, streams, and TLS never read the Ethernet header. The UDP socket calls (`sendmsg`, `IP_PKTINFO`, ECN, GSO, ICMP, real `SocketAddr` values) are the part that cannot be pointed at a raw Ethernet device. That binding is the only new seam:

```text
QUIC core          datagram + opaque peer + max size
    |
Datagram backend
    |-- UDP socket
    +-- Ethernet backend (MAC, EtherType, service id)
```

If a QUIC stack cannot take an opaque peer and an explicit max size, stop. Both ends of this path use the Ethernet backend. QUIC-over-UDP is a second backend, not a peer of this framing.

## Backend contract

| | The backend must | Or else |
| --- | --- | --- |
| 1 | Keep datagram boundaries. One Ethernet payload is one receive. | A stream read splits coalesced packets. |
| 2 | Pass QUIC bytes through, padding included. | AEAD or the 1200-byte Initial rule breaks. |
| 3 | Put a stable peer address on every receive, and accept it on send. | A MAC that changes per frame looks like a new path. |
| 4 | Report max size as MTU minus the adapter header. | The default 65527 invites a jumbo datagram. |
| 5 | Fail an oversize send, and do not transmit it. | A silent drop looks like a successful probe. |
| 6 | Leave ECN unmarked. | A path that requires `IP_TOS` on every datagram delivers nothing. |
| 7 | Skip GSO, GRO, packet info, TTL, and ICMP. | Setup or the MTU search stalls on those calls. |
| 8 | Omit `preferred_address` unless that address is an Ethernet peer. | The peer leaves the L2 path. |
| 9 | Bind Retry tokens and path checks to the address Quinn stored. | A token that holds a real IP fails the next packet. |
| 10 | Demux connections by Connection ID. The MAC is only the path. | One MAC pair cannot hold two connections. |
| 11 | Deliver unicast frames for this MAC, this EtherType, and this service id. | Broadcast burns the anti-amplification budget. |

A 1500-byte payload MTU holds a 1200-byte Initial plus the 2-byte service id. VLAN tags come out of that budget when the socket is bound to the parent device. The adapter does not fragment.

## Address

Above the backend the peer is opaque:

```text
mac: [u8; 6]
service_id: u16    // demux, same job as a UDP port
```

Quinn stores a `SocketAddr`, so the backend encodes a private IPv6 link-local address (MAC in the interface identifier, port = service id). That encoding stays inside the address conversion. It is not handed to the kernel IP stack, not used as a certificate name, and not advertised as `preferred_address`.

One connection uses one peer MAC and one interface. A later source MAC is a new path. Path validation runs. Migration beyond that is out of scope.

## Wire

Development EtherType is `0x88B6` (IEEE local-experimental range `0x88B5`–`0x88B6`).

```text
Ethernet: dst MAC | src MAC | EtherType | payload
payload:  service_id (2 bytes) | one QUIC datagram
```

On Linux the backend is `AF_PACKET` / `SOCK_DGRAM`, bound to the interface and the EtherType. The kernel owns the Ethernet header. The process sees the payload and the peer MAC. Unicast to the host MAC needs no promiscuous mode.

Drop a frame that is short, has the wrong service id, or is not for the local MAC. Do not parse QUIC to decide that.

## Scope

In: two hosts on one L2 segment, many connections demuxed by Connection ID, a UDP backend kept for comparison, MTU from the interface, handshake plus stream data, and tests for framing, loss, reorder, duplication, and oversize sends.

Out: a second packet format, a private congestion or TLS stack, IP routing, Ethernet fragmentation, QUIC extensions, DPDK, and `AF_XDP`. Same-LAN is not trust. QUIC authentication is the security boundary. EtherType, destination MAC, and service id only demux.

## Tests

- Round-trip MAC, EtherType, service id, and QUIC bytes, including empty, 1200, max, and max+1.
- Oversize send returns too-big and the peer sees no frame.
- Two QUIC packets in one payload are one receive.
- On a veth pair with no IP: handshake, one stream, many streams, two connections on one MAC pair, and recovery after loss, reorder, and duplication.
- UDP-backend tests still pass.
- The QUIC packet, recovery, and TLS modules do not name Ethernet, `AF_PACKET`, or EtherType.
