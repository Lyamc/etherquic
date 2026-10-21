# Carrier work

Checked against [plan-mp.md](plan-mp.md), [plan-quic.md](plan-quic.md), [plan-udprtp.md](plan-udprtp.md), [plan-avb.md](plan-avb.md), and [plan-st2110.md](plan-st2110.md).

Already in the tree, and not repeated below: `TLV_CARRIERS`, the `carrier N` offer line and echo, the shared slice assembler, QUIC streams as carrier 1, QUIC datagrams as carrier 2 (including the short-path window and 1 ms ACK delay), UDP payload capped at 1452 with path MTU discovery off, the Linux pcap thread polling the capture fd with the send pipe, and AES-NI through the `aes` crate's runtime check (`aes` 0.8 has no `aesni` feature; do not set `aes_force_soft`).

The carrier items that were open are in the tree.

Connect stores `carrier=N` on the host in `connections.txt` and the next offer carries it. Capture opens `rtp.pcap` for UDP/RTP and ST 2110, and `avtp.pcap` for AVB, each with a dissector. A carrier that is off does not open its file.

Carriers 3, 4, and 5 move picture and audio on their own UDP sockets and do not construct a Quinn endpoint. The host writes its RTP input port as `rtp=` in `connections.txt` and puts that port on its discovery offer. An address Connect asks the host and offers on the port in the reply, keeping that address's IP. A default L2 Connect uses the scanned host's IP and the RTP port already on that offer. An `--l2-offer` connect does not open a raw NIC. It does not use the QUIC service port, the local `rtp=` line, or 127.0.0.1 for a remote host. A missing port fails the view: "The carrier session did not deliver picture and audio." Connect stores the picture the host encoded and hands the PCM to the speaker. A launch to 127.0.0.1:9, with nothing listening, ended on that line for carriers 3, 4, and 5.

A later picture or audio config resets both QUIC datagram assemblers. Cursor updates on carrier 2 leave as datagrams with the picture `D` and `R` records.

UDP/RTP repeats the offer on the input port every 20 ms and answers with one config RTP packet, stopping at 1 s. The default profile is `gcm` from X25519 against the host key, then HKDF. `plain` is the explicit profile. The Connect card, `connections.txt` (`profile=`), and `--profile plain|gcm` set it. The same choice selects ST 2110 `gcm`; otherwise ST 2110 stays `plain`. Picture timestamps use capture time at 90 kHz. Audio timestamps use the configured sample rate. The player drops an incomplete picture after one frame interval (at least 2 ms) and incomplete audio after 20 ms. Keys are retried on the input socket until `EAKU`. Pointer datagrams are sent once. One RTCP receiver report per second is the loss fraction that cuts the encoder cap.

AVB frames come from the `avb` crate. `open1722` writes EtherType `0x22F0`, CVF for picture and AAF for PCM. The experimental subtype is not used. The crate paces at 200 / 2 / 1 Mbit/s, sets a presentation timestamp, and tags VLAN PCP 5 / 3 / 6 only when a VLAN is set. It follows 802.1AS in process and fails the session when the reservation fails. The Connect row says the stream is not encrypted, "AVB (shaped)" before a reservation, and "AVB (reserved)" after one.

ST 2110 frames come from the `st2110` crate. Uncompressed pictures are RFC 4175, one row per packet, paced across the frame. JPEG XS is ST 2110-22 with the marker on the last packet. Audio is ST 2110-30 level A 1 ms PCM. Opus does not announce this carrier. Ports are `l3_port`, `+2`, and `+4`. The default profile is `plain`. A software clock sends when no PTP node is present, and the row says the stream is not locked.

A live AVB switch, a hardware grandmaster, and a PTP appliance stay manual.
