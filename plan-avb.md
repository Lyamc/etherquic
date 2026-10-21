# AVB carrier

IEEE 1722 AVTP on Ethernet (carrier id 4), for a LAN whose switches can reserve bandwidth. L2 only. The operator turns it on. Sending 1722 frames does not make a home switch into an AVB switch.

The quoted class A bound, 2 ms over 7 hops, comes from gPTP and the credit-based shaper on every hop. Stage 1 is the packet format and a software pacer. Stage 2 is the clock and the reservation, which is what makes that bound true. The seam with the session is [plan-mp.md](plan-mp.md). The slice layout is the one in that file.

## Stage 1, shaped Ethernet

EtherType `0x22F0`. The `0x88B6` QUIC socket never sees these frames. The open is the same kind of raw socket the L2 path already uses, bound to `0x22F0`.

One AVTP stream id per lane, from `SENDER_MAC` plus a 16-bit lane: picture 0, audio 1, input 2. Subtype `0x7F` (experimental). This is not a Milan audio stream and the packets do not claim to be one.

The payload is the shared slice header, then the record bytes. The payload cap is the interface MTU minus the Ethernet header and the AVTP header. There is no retransmission. Deadlines and the assembler are the shared ones. Picture and audio are class `deadline`. Keys are class `retry` inside the stream, by sending the same slice again until the deadline. Pointer and pad are class `latest`.

The presentation timestamp is the low 32 bits of a software monotonic clock, set to capture time plus one frame interval. The player presents when its clock reaches that timestamp. A timestamp that is already past is presented immediately if the deadline is still open, and dropped when it is not.

The pacer sends at the reserved rate, one MTU at a time. The operator sets the rates. The defaults are 200 Mbit/s picture, 2 Mbit/s audio, and 1 Mbit/s input. The encoder cap is the picture rate.

A VLAN id is optional. When it is set, picture uses PCP 5, audio PCP 3, and input PCP 6. With no VLAN the frames are untagged, and a switch will not treat them as a priority class. The Connect row says that.

## Stage 2, reserved LAN

After stage 1 is delivering frames.

- gPTP (802.1AS) as a follower. The host becomes the grandmaster only when the LAN has none. Use hardware timestamps when the NIC reports them. Software timestamps stay available, and the UI says the 2 ms bound is not in force.
- MSRP (802.1Qat) talker advertise for the three stream ids, at the stage 1 rates. A failed reservation does not fall back to flooding the LAN. The session fails, and the UI shows the failure the attribute carried.
- Class interval is the shaper's interval, not the packet size. Packets stay MTU-sized. A desktop picture does not fit a 125 µs class A budget, so picture and audio use class B (250 µs). Input uses class A when that reservation succeeds, because a key is one small packet.

## Discovery

Carrier 4 is listed only when the operator enabled AVB on this host. The TLV means this program will send 1722, not that the switch reserved anything. The Connect row says "AVB (shaped)" in stage 1, and "AVB (reserved)" after a reservation has succeeded once on this adapter.

## Tests

Stage 1 loopback on the in-memory fabric with EtherType `0x22F0`. A dropped packet does not stall the next record. Over 100 ms the pacer stays inside the reserved rate. Stage 2 is not unit-tested against a switch. A reserved-LAN check is manual.

## Outside this plan

- Frame preemption, time-gated queues, and Milan certification.
- AVB over Wi-Fi or across a router.
- Encrypting 1722. The reserved LAN is the trust boundary. The UI says the stream is not encrypted.
- Sharing EtherType `0x88B6` with QUIC.
