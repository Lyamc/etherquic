# ST 2110 carrier

SMPTE ST 2110 picture and audio over UDP (carrier id 5), for a link that can carry the rate. A 1080p60 4:2:2 10-bit active picture is about 2.5 Gbit/s before packet overhead. This is not the desktop default.

The carrier is announced only when the operator has enabled it, the selected NIC is 10 Gbit/s or faster (or the operator overrode that check), and the picture this host would actually send is uncompressed or JPEG XS. MJPEG, PyroWave, H.264, H.265, and AV1 do not announce it.

RTP sockets, RTCP-mux, and the `plain` / `gcm` profiles come from [plan-udprtp.md](plan-udprtp.md). The session seam is [plan-mp.md](plan-mp.md). The opaque-record slice header is not used here.

## Essences

| Essence | Document | Port |
| --- | --- | --- |
| Picture | ST 2110-20 uncompressed, or ST 2110-22 JPEG XS | `l3_port` |
| Audio | ST 2110-30 PCM | `l3_port + 2` |
| Input | The input datagrams from the UDP/RTP plan | `l3_port + 4` |

RTCP shares the essence port. QUIC is not bound on these ports for this session. There is no NMOS registration and no SDP file. The version-3 TLV and the offer line are the session description.

Opus is not an ST 2110 audio essence. When the negotiated audio is Opus, carrier 5 is not offered.

## Picture

Uncompressed video is RFC 4175. One packet holds at most one row. Packets are paced linearly across the frame (ST 2110-21 linear). The decoder still receives one picture. The carrier packetizes the raster.

JPEG XS uses the ST 2110-22 RTP payload. The marker bit is set on the last packet of the frame. Packets are paced across the frame's send window rather than leaving in one burst.

The RTP timestamp is the ST 2110 media clock. The capture timestamp inside the session stays the EQIN microsecond clock. The carrier maps between them. A packet that arrives after the first packet of the next frame is dropped. There is no retransmit and no NACK.

## Audio

ST 2110-30 level A. 1 ms PCM packets, at the session's channel count and rate.

## Clock and pacing

ST 2110-10. Follow PTP (IEEE 1588) when the NIC or the OS has a clock. The host does not run a grandmaster. A software clock may still send, and the UI says the stream is not locked.

The pacer uses that clock. It does not use CUBIC or the QUIC pacer. The encoder cap is the essence rate: fixed for an uncompressed raster, and the JPEG XS target otherwise. If the send falls behind by more than one frame, drop the frame in hand and send the next one on time. Do not grow a queue.

## Encryption

`plain` or `gcm`, as the UDP/RTP plan defines them. `gcm` covers the RTP payload. Gear that expects cleartext ST 2110 will not decode `gcm`. The default on this carrier is `plain`, and the UI says so.

## Tests

A 64×64 raster becomes RFC 4175 packets that cover every sample once, with the marker bit on the last packet of the frame. Those packets' timestamps span one frame, and the pacer does not emit them in one burst. No switch and no grandmaster in the unit tests.

## Outside this plan

- ST 2110-40 ancillary data.
- NMOS IS-04 and IS-05.
- Changing the MJPEG or PyroWave encoders so they can feed this carrier.
- Sending ST 2110 on the `0x88B6` socket.
