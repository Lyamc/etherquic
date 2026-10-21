# QUIC carrier

The default carrier. Two modes on the sockets that already exist (`src/l2_endpoint.rs`, `src/udp_endpoint.rs`). Mode 1 is the session that ships today. Mode 2 keeps the Quinn connection and moves picture and audio onto datagrams. The seam with the other carriers is [plan-mp.md](plan-mp.md).

## Mode 1, streams (id 1)

No wire change. Unidirectional streams, send priority trace 30, input 20, audio 10, video 0. Records stay length-prefixed on `EVID` / `EAUD` / `EINP`, or on `EVI2` / `EAU2` when the ALPN is `etherquic/2`.

This is what an old peer speaks. A new peer uses it until both sides agree on datagrams.

Three local fixes. None of them change bytes on the wire, and none of them grow the congestion window:

| Knob | Today | Change |
| --- | --- | --- |
| Linux L2 wait | `recv_timeout(1 ms)`, then read the NIC | Wait on the pcap fd and the send queue together, so a picture is not held for that timeout. |
| UDP MTU discovery | on, initial payload 1452 | Off for a LAN dial. 1452 stays the cap. L2 already pins 1494 and disables discovery. |
| AES | vendored `aes` built without `aesni` | Enable `aesni` in that provider. Still RustCrypto. Still no `ring`. |

Leave CUBIC's 12,000-byte initial window and the 333 ms initial RTT in mode 1. A larger burst is mode 2, and only on a short path.

## Mode 2, datagrams (id 2)

Same handshake, same ALPN, same connection. Picture and audio use Quinn datagrams. The offer stream, the input stream, and `EACK` stay reliable streams.

The offer is read and the config record is written on the reliable stream before the first picture datagram. A later config record resets the assembler. The next picture after a config is a full frame, which is already the dirty-rect rule.

A datagram is the [plan-mp.md](plan-mp.md) slice with a lane byte in front:

```text
lane u8 | record_id u16le | index u16le | count u16le | bytes
```

`lane` is 1 for picture and 2 for audio. Slice size is the QUIC datagram budget, minus the datagram frame overhead. The receiver uses the shared assembler. A hole does not hold the next record. Quinn does not retransmit a datagram.

Input stays a stream. Pointer folding still happens before the bytes are written. A lost input packet can still delay later input for one recovery time. Moving pointer samples to datagrams waits until picture datagrams have been measured.

## Window and ACKs (mode 2, short path only)

Datagrams are not retransmitted. The congestion window still decides how fast they leave. On L2, and on UDP after the first RTT sample is under 2 ms:

| Knob | Value |
| --- | --- |
| `initial_rtt` | 1 ms |
| CUBIC initial window | 256 KiB |
| `AckFrequencyConfig` max ACK delay | 1 ms |
| ACK-eliciting threshold | 0 (ACK every packet) |

Quinn rejects a max ACK delay under its 1 ms timer granularity, so 1 ms is the floor. Both ends set it. The peer already advertises a 1 ms minimum ACK delay.

On UDP whose first RTT is 2 ms or more, keep the 12,000-byte window and the 333 ms initial RTT. A 256 KiB burst onto a slow or lossy path is how mode 2 loses whole frames.

One missing datagram does not cut the send rate. The loss fraction over the last second does. Above 1% for a full second, the encoder cap reported to the session drops by 20% and stays down until a second with no loss. Below that, the incomplete frame is dropped and the rate stays.

## Compatibility

Carrier 2 is used only when `TLV_CARRIERS` lists it and the offer says `carrier 2`. A stream-only peer is never sent a datagram. Mode 1 tests stay as they are and must not start emitting datagrams.

## Tests

On the in-memory fabric:

- A record larger than one datagram arrives whole.
- Dropping the first slice, and dropping the last slice, each yield no record, and the next record still arrives.
- A mode 1 connection completes a picture without a datagram.

## Outside this plan

- Replacing Quinn, or changing EtherType `0x88B6` and the service header.
- FEC, and BBR. The short-path profile is a large window plus the encoder cap.
- Datagram input.
