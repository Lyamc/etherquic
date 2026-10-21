# One session, several carriers

Capture, encode, the offer, and the picture records stay. A carrier only moves finished records. QUIC streams stay the default. QUIC datagrams, UDP/RTP, AVB, and ST 2110 are further carriers. Each has its own plan. This file is the seam.

| Carrier | Id | Plan |
| --- | --- | --- |
| QUIC streams (today) | 1 | [plan-quic.md](plan-quic.md) |
| QUIC datagrams | 2 | [plan-quic.md](plan-quic.md) |
| UDP/RTP | 3 | [plan-udprtp.md](plan-udprtp.md) |
| AVB / IEEE 1722 | 4 | [plan-avb.md](plan-avb.md) |
| ST 2110 | 5 | [plan-st2110.md](plan-st2110.md) |

## What stays put

- `media/` encode and decode, dirty rectangles, quality, and frame rate.
- Record bodies. `EVID` / `EVI2` pictures, `EAUD` / `EAU2` audio, `EINP` documents, and `EACK` when input tracing is on. A carrier may slice a record. It does not invent a second picture format.
- EQIN events, and the pointer and pad fold in the outbox.
- Discovery. Magic `EQDS`, service `4440`, version 1–3 offers. An old player still receives the offer it already accepts.

## Carrier

`play/src/session.rs` talks to a carrier. It does not open Quinn streams once a carrier is selected. Stream open stays inside the QUIC carrier.

```text
host:   encoded record → carrier.send(lane, bytes, deadline, class)
player: carrier.recv() → the same record bytes the QUIC reader hands the decoder today
```

`lane` is picture, audio, or input. `deadline` is the latest time the player can still use the record. The host capture clock is the `u64` microsecond timestamp EQIN already writes. A carrier with PTP or gPTP maps that clock. It does not replace the timestamp inside the record.

## Reliability

The session asks for a class. The carrier implements it or refuses the session.

| Lane | Class | Behaviour |
| --- | --- | --- |
| Picture | `deadline` | A late or incomplete record is dropped. Later records are not held behind it. |
| Audio | `deadline` | Same. The gap is silence or concealment. |
| Keys | `retry` | Retransmit until the deadline, then drop that event. Key order is kept. |
| Pointer and pad | `latest` | A newer sample replaces an older one still queued. |

Carrier 1 is stricter than this: reliable and ordered, no deadline. That is the session that already ships. Carriers 2–5 implement the table.

## Slice header

Carriers 2, 3, and 4 slice an opaque record. One assembler covers all three. ST 2110 does not use this header. It packetizes the raster or a JPEG XS RTP payload, as [plan-st2110.md](plan-st2110.md) describes.

```text
record_id u16le | index u16le | count u16le | bytes
```

`index` is 0-based. `count` is the number of slices. The last slice is `index + 1 == count`. A record is delivered when every index is present. It is dropped when the deadline passes, or when a newer `record_id` arrives while this one still has a hole. `record_id` wraps at 65535. A jump larger than half the space starts a new epoch.

QUIC datagrams name the lane in an extra leading byte because every datagram shares one connection. RTP and AVTP omit it because the socket or the stream id is the lane.

## Discovery

Version-3 offers already carry TLV records, and unknown types are skipped. Add one type. Do not bump the offer version. An unmarked announce still gets a version-1 or version-2 offer.

| Field | Value |
| --- | --- |
| TLV type | `4` (`TLV_CARRIERS`) |
| Value | count `u8`, then one `u8` id per carrier, at most 8 |

A missing TLV means carrier 1 only.

The player writes the chosen id on the offer, a line an old host ignores:

```text
carrier 1
```

A host that agreed on ALPN `etherquic/2` echoes the id on the config record. A mismatch closes the session before the first picture. ALPN `etherquic` is always carrier 1.

## Connect

The Layer row stays: L2, IPv4, IPv6. A Carrier row lists the intersection of what this machine can run and what the host announced. The default is QUIC streams. A pin is stored by host MAC in `connections.txt`, beside the codec pin. An empty pin means carrier 1.

| Carrier | L2 | UDP |
| --- | --- | --- |
| 1 QUIC streams | yes | yes |
| 2 QUIC datagrams | yes | yes |
| 3 UDP/RTP | no | yes |
| 4 AVB | yes | no |
| 5 ST 2110 | no | yes |

Android has no L2, so AVB is absent there. AVB and ST 2110 are also gated by the operator switch in their own plans. Announcing a carrier this machine cannot open is a bug.

The base UDP port is the announced `l3_port`. Carrier plans own offsets from that port. A carrier whose port is already taken is left off the offer.

## Rate

The encoder already has a target from quality and the codec. The carrier reports one number up: a cap in bits per second, and the loss fraction over the last second. Reservations (AVB, ST 2110) report the reserved rate. Loss handling inside that cap belongs to the carrier plan. No carrier changes GOP or dirty-rect mode.

## Capture

`app.pcap` stays the plaintext records for every carrier. `ether.pcap` stays the `0x88B6` frames. UDP/RTP and ST 2110 also write `rtp.pcap`. AVB writes `avtp.pcap` as Ethernet. The dissector grows one decoder per file. A carrier that is off does not open its file.

## Tests

One loopback per carrier, with one fixed picture record and one fixed PCM record.

1. A clean send delivers the bytes the QUIC reader would hand the decoder.
2. One dropped packet on a `deadline` carrier yields no partial record and does not block the next record.
3. The same drop on carrier 1 still delivers the record after a retry.
4. A version-3 offer with an unknown TLV still parses. A missing `TLV_CARRIERS` selects carrier 1.

## Order

1. The carrier trait, `TLV_CARRIERS`, the offer line, and carrier 1 behind the trait. No wire change for current peers.
2. QUIC datagrams. Same sockets, different recovery.
3. UDP/RTP, including the slice assembler the later carriers share.
4. AVB, then ST 2110.

## Outside this plan

- A second picture format.
- Two carriers in one session. Picture and audio do not split.
- NMOS, Milan certification, or any protocol that configures the switch.
