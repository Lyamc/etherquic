# Wireshark

`etherquic.lua` decodes EtherType `0x88B6` and the desktop records in a user-DLT capture.

```bash
wireshark -X lua_script:wireshark/etherquic.lua ether.pcap app.pcap
```

The preferences panel writes both files when Capture is on. `ETHERQUIC_CAPTURE=DIR` does the same for the window and for `etherquic-play`. The directory is not the preferences file. Capture stays off until it is turned on, and the switch is not saved.

`ether.pcap` is Ethernet. Each frame is one this process sent or received on the `0x88B6` socket. The service header is destination service, source service, and payload length. The payload is a QUIC datagram or an EQDS discovery message. A sent frame can also show up as a receive if the NIC echoes it. Header protection hides the QUIC packet number.

`app.pcap` is link type 147 (`USER0`). The record is direction, three zero bytes, a four-byte magic (`EVID`, `EAUD`, `EINP`, `EACK`), then the payload. An empty payload is the stream opening. `EVID` is a JPEG. `EAUD` is sample rate, channel count, then interleaved `i16` PCM. `EINP` is an EQIN document. `EACK` is a 15-byte input confirmation. These records are the plaintext after QUIC, so a UDP session still writes `app.pcap`. The file-send example is not in this capture.

`rtp.pcap` (link type 148, `wireshark/rtp.lua`) is opened only while the session carrier is UDP/RTP or ST 2110. `avtp.pcap` (Ethernet, `wireshark/avtp.lua`, EtherType `0x22F0`) is opened only while the carrier is AVB. The other carrier does not create its file.
