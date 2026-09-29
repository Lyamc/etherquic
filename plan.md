# QUIC over Ethernet

## Will it work if QUIC is unaware that it is right on top of Ethernet?

Yes, if the backend presents four things QUIC already requires from UDP, and withholds the IP-specific socket features QUIC can live without.

QUIC will work while remaining unaware of Ethernet when every send and receive looks like this:

1. One call carries one datagram. The bytes are one or more coalesced QUIC packets, and the boundary is the boundary of the Ethernet payload.
2. Every datagram has a peer address that is stable for the life of that path, comparable, and usable as a send destination.
3. The backend tells QUIC the maximum datagram size, and a larger send fails before anything is put on the wire.
4. A 1200-byte Initial datagram fits in one frame.

QUIC's packet protection does not cover the UDP header or the IP pseudo-header. Connection IDs, not the 4-tuple, identify a connection. Loss detection and congestion control use packet numbers and ACK timing. Stream state and TLS state never read the lower-layer header. None of that machinery has to learn what a MAC address is.

The UDP *socket integration* cannot be pointed at a raw Ethernet device and left unaware. `sendmsg` / `recvmsg`, `IP_PKTINFO`, `IP_TOS` / ECN, `UDP_SEGMENT` (GSO), ICMP errors, and `SocketAddr` values that must be real IP addresses are properties of the UDP binding. The Ethernet backend replaces that binding. Above the binding, the QUIC implementation stays as it is.

So the split this plan commits to is:

```
Application
    │
    ▼
QUIC core                         unaware of Ethernet
(TLS, streams, packets, CIDs,
 loss detection, congestion
 control, connection state)
    │  datagram + opaque peer + max size
    ▼
Datagram backend                  the only new seam
    │                    │
    ▼                    ▼
UDP socket            Ethernet backend
(IP, ports, ECN,      (MAC, EtherType,
 GSO, ICMP)            service id, AF_PACKET)
```

If the chosen QUIC code cannot accept a datagram backend with an opaque peer address and an explicit max size, stop. Write down which assumption leaked, in `docs/networking.md`, before adding Ethernet conditionals to the QUIC core.

This transport is experimental. Both ends run this Ethernet backend. Ordinary QUIC-over-UDP peers are a separate backend, not a peer of this framing.

## What "unaware" requires of the backend

These are the obligations of the Ethernet backend. Each one is a test, not a slogan.

| # | Obligation | Why QUIC still functions | How it fails |
|---|---|---|---|
| 1 | Preserve datagram boundaries. One Ethernet payload equals one QUIC receive buffer. | Coalesced packets are parsed using the datagram length. | A stream-shaped read splits or merges packets and the codec fails. |
| 2 | Pass QUIC bytes through unchanged, including padding. | The 1200-byte Initial minimum and AEAD both apply to those bytes. | Stripping padding breaks the amplification rule. Rewriting bytes breaks AEAD. |
| 3 | Attach a stable peer address on receive, and accept that address on send. | Retry tokens, anti-amplification (3×), stateless reset, and path validation key off the address the stack sees. | A MAC that changes every frame looks like a new path on every packet. |
| 4 | Report `max_datagram_size` from the interface MTU minus the adapter header. | QUIC will not send a larger datagram when the path cap is visible. Initial packets are at least 1200 bytes and fit in a 1500-byte Ethernet payload. | Advertising 65527 (`max_udp_payload_size`'s default) invites the peer to send a jumbo QUIC datagram. |
| 5 | Reject an oversize send with a distinct too-big error. The frame is not transmitted. | Datagram packetization layer PMTU discovery falls back when a probe fails. | A driver that drops the frame while `send` returns success makes the probe look acked-as-sent. The sender keeps using a size the path cannot carry. |
| 6 | Leave ECN unmarked. | ECN is optional. Unmarked traffic is valid QUIC. | A receive path that requires `IP_TOS` ancillary data on every datagram never delivers a packet. |
| 7 | Skip GSO, GRO, IP packet info, TTL, and ICMP. | Loss recovery already treats loss, reorder, and duplication. | A hard dependency on `UDP_SEGMENT` or on ICMP "packet too big" stalls the socket setup or the MTU search. |
| 8 | Suppress `preferred_address` (and any other transport parameter that names an IP) unless the named address is an Ethernet peer this backend can reach. | The parameter is optional. | An advertised IP sends the peer off the L2 path. |
| 9 | Bind Retry tokens and path-validation state to the peer address the QUIC stack actually stored. | Address validation only needs a round trip to that address. | A token that stores a real client IP, while the stack is sending to a MAC, fails validation on the next packet. |
| 10 | Demultiplex connections with QUIC Connection IDs. The MAC is only the path address. | This is the QUIC rule on UDP as well. | Using the MAC as the connection identifier breaks multipath-style mobility, stateless reset, and multiple connections between the same pair of hosts. |
| 11 | Deliver only unicast frames for this host's MAC, this EtherType, and (when configured) this service id. | QUIC paths are unicast. | Broadcast or foreign EtherTypes reach the codec and waste the anti-amplification budget. |

Standard Ethernet satisfies the size floor. The usual interface MTU is 1500 bytes of payload above the 14-byte Ethernet header. The FCS stays in the NIC. A 1200-byte Initial plus a 2-byte service id is 1202 bytes of payload. VLAN tags, if the socket is bound to the parent device rather than the VLAN device, come out of that budget. Jumbo interfaces raise the cap to their MTU minus the adapter header. The adapter still does not fragment.

## Address model

The peer address is opaque above the backend.

```
EthernetPeer {
    mac: [u8; 6],
    service_id: u16,   // present when multiple listeners share a segment
}
```

`service_id` plays the role a UDP port plays for demultiplexing listeners. It is an adapter field. It is not a UDP port and it is not part of the QUIC packet.

The first version may use the source MAC alone when a single listener per host is enough. The documented identity of a path is then `(source MAC, service_id)`.

If the chosen QUIC API stores only `SocketAddr`, a single stable encoding may be used:

- IPv6 link-local address with the MAC in the interface identifier
- port field equal to `service_id`

That encoding is a private detail of the backend. It stays inside the address conversion functions. It is never passed to the kernel's IP stack, never used as a certificate identity, and never advertised as `preferred_address`. Subnet checks, multicast checks, and link-local scope-id checks in the QUIC code are inspection items. Any of those rejecting the encoded address is a leaked assumption and is recorded before the encoder is trusted.

Connection migration stays off in the first version. One connection, one fixed peer MAC, one fixed interface. A later note records what the stack does if the source MAC changes. The expected behavior of an unaware stack is: a new MAC is a new path, and path validation runs against that peer address. Sophisticated migration is out of scope until that behavior is written down.

## Wire format

EtherType is configurable. Development uses a locally assigned experimental value, not a registered one.

```
Ethernet frame
┌──────────────────┬──────────────────┬───────────┬────────────────────────┐
│ Destination MAC  │ Source MAC       │ EtherType │ Payload                │
│ 6 bytes          │ 6 bytes          │ 2 bytes   │ service_id + QUIC data │
└──────────────────┴──────────────────┴───────────┴────────────────────────┘

Payload
┌────────────┬──────────────────────┐
│ service_id │ QUIC datagram        │
│ 2 bytes    │ one or more coalesced│
│            │ QUIC packets         │
└────────────┴──────────────────────┘
```

On Linux the first backend is `AF_PACKET` with `SOCK_DGRAM`. The kernel builds and strips the Ethernet header. The process reads and writes the payload, and passes the peer MAC in `sockaddr_ll`. Bind the socket to the interface index and the EtherType so the kernel filters other types. Unicast frames addressed to the host MAC arrive without promiscuous mode.

`SOCK_RAW`, DPDK, and `AF_XDP` are later options. The datagram contract above them stays the same.

Receive path:

```
NIC
  → AF_PACKET
  → payload
  → drop if the frame is short, the service id does not match, or the destination MAC is not local
  → remaining bytes + source MAC
  → QUIC receive
```

The backend does not parse QUIC headers to decide acceptance. Logging at this layer records peer MAC, length, and the drop reason. Packet numbers stay in the QUIC logs.

## Scope

In scope:

- Two endpoints on the same L2 segment.
- Several QUIC connections at once, demultiplexed by Connection ID.
- A UDP backend that remains usable so the same QUIC core can be compared.
- Configurable interface, source MAC (or the interface MAC), EtherType, service id, and peer MAC.
- MTU derived from the interface.
- Counters for sent, received, dropped, and too-big.
- A client and a server that complete a handshake and transfer stream data.
- Tests for framing, the backend contract, handshake, streams, loss, reorder, duplication, and oversize sends.

Out of scope for the first version:

- A second QUIC packet format, a private congestion controller, or a private TLS stack.
- IP routing, ARP, IPv4, IPv6, and cross-subnet operation.
- Ethernet fragmentation or reassembly.
- QUIC protocol extensions.
- Hardware offload, DPDK, and `AF_XDP`.
- Production hardening of L2 admission. Same-LAN is not trust. QUIC's cryptographic authentication remains the security boundary. EtherType, destination MAC, and service id are demultiplexing filters.

## Work order

### 1. Find the real seam

Pick the QUIC implementation this tree will embed. Before writing the backend, produce `docs/networking.md` with:

- The send and receive path as it exists (socket creation, `sendmsg` / `recvmsg`, address types, port handling).
- Where MTU, ECN, GSO/GRO, packet-info ancillary data, path selection, migration, and stateless reset hook in.
- The type that stores a local address, a remote address, and a path.
- The insertion point for a second datagram backend.
- Every assumption that the adapter cannot hide, classified as:
  - hidden by the datagram contract
  - handled by configuration (disable GSO, leave ECN off, omit `preferred_address`, clamp max datagram size)
  - a change inside the QUIC core

Estimate the core diff. If the third class spreads through packet processing, TLS, loss detection, or congestion control, stop and extend `docs/networking.md` with the reason. The rest of the phases wait on that document.

### 2. Frame codec

Encode and decode the payload: `service_id` plus opaque QUIC bytes. Pure functions, no sockets, no QUIC state machine. Tests are listed below.

### 3. AF_PACKET backend

Open the interface, send, receive, filter EtherType, and surface source and destination MAC. Enforce `max_datagram_size`. Map too-big and other send failures onto the errors the QUIC stack already handles for UDP, with too-big kept distinct.

### 4. Connect the backend

Wire QUIC's existing datagram API to the Ethernet backend. Keep the UDP backend compiling and passing its tests. QUIC core files gain an alternate backend, or a generic address type at the existing socket trait, and nothing else.

### 5. Handshake

Client and server on one L2 segment, no IP and no UDP. TLS finishes. The Connection IDs in the captured frames are the connection identifiers. The MACs in the Ethernet header are the path.

### 6. Data

One bidirectional stream, several streams, and a transfer larger than a handful of frames.

### 7. Datagram faults

Drop, reorder, and duplicate Ethernet frames. QUIC's existing recovery completes the transfer. Separately, offer a send larger than `max_datagram_size` and confirm the too-big error and that no frame was sent. Confirm a stack configured for a 1200-byte start does not stall with ECN, GSO, and ICMP unavailable.

### 8. Write the measured answer

Update `docs/quic-over-ethernet.md` with the architecture that was built and a list of every QUIC-core change. Answer, from the diff and the tests:

- Which UDP/IP dependencies the adapter hid.
- Which ones were configured off.
- Which ones required a core change.
- Whether a QUIC stack with no Ethernet knowledge in its packet, TLS, recovery, or stream code completed the handshake and the loss tests.

That write-up is the empirical answer to the question at the top. The protocol-level answer in this file stands either way. The codebase either met the contract or it did not.

## Tests

Framing:

- Round-trip of MAC, EtherType, service id, and QUIC bytes.
- Empty payload, 1200-byte payload, maximum payload, and one byte over the maximum.
- Short frame, mismatched EtherType, mismatched service id, destination MAC that is not local.
- QUIC bytes at the end of the payload compare equal to the input, byte for byte.

Backend contract:

- `send` then `receive` returns the same bytes and the same peer.
- Oversize `send` returns too-big and the receiver observes no frame.
- A coalesced buffer (two QUIC packets in one payload) is one receive.

Integration, on a veth pair or the equivalent, with IP addresses unconfigured on those interfaces:

- Handshake completes.
- One bidirectional stream.
- Several streams, including a transfer well above the MTU.
- Two connections between the same MAC pair, distinguished by Connection ID.
- Injected loss, reorder, and duplication still deliver the stream data.
- UDP-backend tests still pass.
- The QUIC crate's packet, recovery, and TLS modules do not reference Ethernet, `AF_PACKET`, or EtherType. A dependency check or a review rule enforces this.

## Configuration

Shape this to the host project. Conceptually:

```
transport: ethernet
interface: eth0
source_mac: auto
ether_type: 0x88B5
service_id: 4433
peer_mac: "02:00:00:00:00:02"
```

`0x88B5`–`0x88B6` are the IEEE local-experimental EtherTypes. Pick one explicitly in configuration so a capture filter can match it. Confirm the value against the current IEEE registry when the first frame hits a wire outside a lab.

```
server --transport ethernet --interface eth0 --service-id 4433
client --transport ethernet --interface eth0 --peer-mac 02:00:00:00:00:02 --service-id 4433
```

## Capture

A filter on the experimental EtherType shows destination MAC, source MAC, service id, and the QUIC datagram. Document the tcpdump (or equivalent) invocation in `docs/quic-over-ethernet.md`.

## Acceptance

- Two hosts on one Ethernet segment complete a QUIC handshake and a bidirectional stream with no IP and no UDP on that path.
- Controlled frame loss and reordering are recovered by the existing QUIC machinery.
- Several streams share one Ethernet-backed connection, and two connections share one MAC pair.
- A datagram above the backend maximum is rejected and is not fragmented.
- The UDP backend's tests still pass.
- Ethernet types appear only in the backend and the frame codec.
- `docs/networking.md` records the seam that was found. `docs/quic-over-ethernet.md` records the core diff and the measured answer to the question at the top.

## Deliverables

- `docs/networking.md` — inspection of the chosen QUIC stack and the classified assumptions.
- `docs/quic-over-ethernet.md` — built architecture, capture notes, and the measured answer.
- Ethernet frame codec, `AF_PACKET` backend, and the datagram-backend wiring.
- Client and server examples.
- Framing tests, backend-contract tests, and the integration tests above.
