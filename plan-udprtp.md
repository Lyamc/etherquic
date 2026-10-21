# UDP/RTP carrier

Picture and audio as RTP over UDP (carrier id 3). This carrier does not open a Quinn connection. Input is a sibling datagram protocol on its own port. Discovery is still `EQDS`, which is already outside QUIC. The shared assembler, classes, and offer line are [plan-mp.md](plan-mp.md).

## Ports

One UDP socket per lane, on the host's announced `l3_port`:

| Socket | Port |
| --- | --- |
| Picture RTP and RTCP | `l3_port` |
| Audio RTP and RTCP | `l3_port + 1` |
| Input | `l3_port + 2` |

RTCP shares the RTP socket (RFC 5761). The even/odd port rule is not used, because `l3_port` is whatever the host bound. If any of the three ports is taken, carrier 3 is left off the offer.

## RTP

A 12-byte RTP header, then the [plan-mp.md](plan-mp.md) slice. The lane is the socket, so the slice has no lane byte.

| Field | Picture | Audio |
| --- | --- | --- |
| Payload type | 96 | 97 |
| Clock | 90 kHz | The rate in the audio config |
| Timestamp | Capture time on that clock | The same capture time |
| Marker | Set on the last slice of a record | Set on the last slice of a record |
| SSRC | Random, stable for the session | A second random SSRC |

The picture deadline is one frame interval, and at least 2 ms. The audio deadline is 20 ms. A slice for an older `record_id` after the deadline is ignored.

## Session start

The codec offer cannot ride a QUIC stream here. The player repeats the offer record on the input port every 20 ms. The host answers on the picture port with the config record as one RTP packet (payload type 96) before any picture. The player stops repeating once that config arrives, and gives up after 1 s.

The offer and the latest config are the only records that are repeated. Picture and audio records are not. An old QUIC-only host does not bind these ports. The player dials them only after it picked carrier 3.

## Encryption

The Connect card picks the profile. The default is `gcm`.

| Profile | Bytes |
| --- | --- |
| `plain` | RTP payload in the clear. The user has to choose this. |
| `gcm` | AES-128-GCM over the RTP payload. The RTP header stays in the clear. |

`gcm` uses the host key that QUIC already loads. Before the offer, the player sends a 32-byte X25519 public key on the input port. The host answers with its own public key and a signature over the transcript from that host key. Both sides derive the AES-128 key with HKDF. Then the offer exchange above starts.

The 12-byte nonce is the SRTP roll-over construction: the 32-bit SSRC, the 32-bit roll-over counter, and the 16-bit sequence, with two zero bytes. The counter advances when the sequence wraps. A repeated nonce closes the session.

## Input

Not RTP. One datagram on the input port.

Pointer and pad use class `latest`. The outbox fold still runs first. Those datagrams are not retransmitted.

Keys use class `retry`. Each key datagram carries a `u16` id. The host replies on the same port with magic `EAKU` and that id. The player resends an unacked key every 2 ms and drops it at the picture deadline. The host applies keys in id order. A gap waits until that deadline, then skips the missing id. A dropped key is a missed key. It does not stall the next one.

## Rate

There is no socket congestion controller. Until the path has been measured, the encoder cap is 100 Mbit/s. After that it is 80% of the measured rate. A loss fraction above 1% for a full second cuts the cap by 20%, and a clean second restores it. The same rule as QUIC datagrams.

A receiver report goes out once a second on the picture socket and once a second on the audio socket. Those reports are the loss fraction. Sender reports wait.

## Tests

An in-memory UDP pair:

- A sliced record reassembles to the original bytes.
- One dropped slice does not block the next record.
- A key is resent until `EAKU`. A pointer datagram is sent once.
- `gcm` round-trips, and a second packet with the same sequence and roll-over counter fails the session.

## Outside this plan

- FEC, NACKs, GCC, and transport-wide congestion control.
- The full SRTP profile. `gcm` is the exchange above.
- RTP on EtherType `0x88B6`.
