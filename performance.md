# Latency

A smooth 60 fps picture has 16.7 ms from the host display to the player window. 120 fps has 8.3 ms. 144 fps has 6.9 ms. 240 fps, the highest rate an offer accepts, has 4.2 ms. The budget is one frame. A second frame of codec delay, or a link that cannot empty the picture inside that window, is already late.

Rank 1 in each table is the choice that can feed 60 fps. The same tables are the Carrier `?` control: hover is this budget, click opens the rows.

The flowchart is [`docs/latency.d2`](docs/latency.d2). Rendered: [`docs/latency.svg`](docs/latency.svg).

## What is holding 60, 120, and 144

1. **Auto encode is 15 fps for software and 30 fps for hardware.** A 60 Hz display still encodes at 30 until the player asks for 60, 120, or Match. Match follows the host display, capped at 240. This is the usual reason a session never reaches 60.

2. **Capture and present can each wait one refresh.** Capture sleeps for `1000/fps` milliseconds, and at least 1 ms, then grabs one image. Integer division makes 60 into 16 ms, 120 into 8 ms, and 144 into 6 ms. The grab asks the display server for the monitor list and one image every period. The player draws through softbuffer. On a 60 Hz compositor those two waits are 33 ms before the codec and the network are counted. 120 and 144 need a display that refreshes that fast, and Match or an explicit rate.

3. **QUIC streams keep Quinn's defaults.** Max ACK delay is 25 ms. CUBIC's first window is 14,720 bytes. The 12,000-byte mode-1 constant is not written onto that transport. A 100 kB picture is about seven flights. On the 5–16 ms ping from the NDIS comparison below, that is 35–112 ms before the frame is out. One lost packet stalls the reliable stream until it is recovered. Streams are the right carrier for a long path and for an old peer. They are the wrong carrier for 120 and 144.

4. **QUIC datagrams are the short path.** On L2, and on UDP whose minimum RTT is under 2 ms, the connection moves to a 1 ms ACK delay, an ACK on every packet, and a 256 KiB window. Quinn's timer will not go under 1 ms. A missing datagram drops that picture and the next one still arrives. UDP at 2 ms or more stays on the 333 ms initial RTT and the 12,000-byte window. The first sample is what decides. A stored sample from another connection does not.

5. **Npcap cannot feed the frame.** On one 1 Gbit/s link, after Membrane was stopped, a 2 MB copy took 21.6 s (0.09 MiB/s). A 50 kB picture at that rate is about half a second. Membrane on the same copy took 0.42 s (4.8 MiB/s), about 10 ms for 50 kB, and a later 1 ms idle wait moved it to 6.3–7.7 MiB/s. The gap is one system call per frame. Linux does not use Npcap. Its L2 thread polls the pcap fd and the send wake together, so a picture is not held for the 1 ms timeout that the other platforms still use. macOS receives on BPF and sends on AF_NDRV.

6. **The codec catalog is in frames, not a stopwatch on this machine.** PyroWave, MJPEG, and JPEG XS are about one frame. OpenAPV and MJ2K are about one to two. H.264 is about two, with B-frames off. H.265 is about two to three and wants an encode ASIC. AV1 is about three to four and wants an encode ASIC. At 60 Hz, two frames is 33 ms. At 144 Hz, one frame is already the whole budget, so a catalog "~1 frame" codec has no time left for capture, the link, or present. Hardware H.264 is the practical 60 fps codec when the link is too small for PyroWave. AV1 is the smallest stream, not the 120 fps one.

7. **AVB spreads the picture.** Class B credit is 250 µs. The picture is shaped at 200 Mbit/s, which is 25 kB per millisecond. A 50 kB picture takes 2 ms of shaping. A 200 kB picture takes 8 ms, which is the entire 120 Hz frame. Uncompressed 1080p60 is about 2.5 Gbit/s and does not fit. A failed reservation ends the session. The row says the stream is not encrypted.

8. **ST 2110 spends the frame on purpose.** Uncompressed rows and JPEG XS packets are paced from the first packet to the last across one frame. Audio is 1 ms PCM. Opus does not announce this carrier. A send more than one frame behind drops the frame in hand. Announce wants a NIC of at least 10 Gbit/s, unless that check is overridden, and a picture that is uncompressed or JPEG XS.

9. **Audio is its own lane.** Opus encodes 10 ms at 48 kHz in restricted-low-delay mode. That is longer than one 144 Hz picture and it does not hold the picture. An incomplete UDP audio record waits 20 ms. An incomplete UDP picture waits one frame interval, and at least 2 ms. One RTCP receiver report per second is the loss fraction that cuts the encoder cap. It is not a per-frame clock.

10. **Frame generation does not shrink the real frame.** It draws a synthesised picture between arrivals. The real frames are still one capture period apart.

## Where the milliseconds go

| Stage | What the code does | Time |
| --- | --- | --- |
| Display wait, capture | Sleep `1000/fps` ms, then one grab | 16 ms at a 60 request, 8 ms at 120, 6 ms at 144, plus whatever the grab blocks |
| Display wait, present | softbuffer on the compositor | Up to one refresh. Not measured here |
| Fit, look, dirty compare | On the captured RGB before encode | Not measured here. Unchanged pictures send nothing |
| Encode | Catalog ranks above | ~1 frame for PyroWave, MJPEG, JPEG XS. ~2 for H.264. ~3–4 for AV1 |
| QUIC streams in flight | 14,720-byte start window, ACK at least every two packets, 25 ms cap when packets are sparse | About seven flights for a 100 kB picture |
| QUIC datagrams, short path | 256 KiB window, 1 ms ACK floor | One flight, plus 1 ms of ACK timer |
| QUIC datagrams, long UDP | First RTT 333 ms until a sample under 2 ms | Startup, then the long profile stays |
| Linux L2 | `poll` on the pcap fd and the send wake | No added 1 ms sleep |
| Other L2 | Send queue waits 1 ms, then a batch read | Up to 1 ms when nothing is queued |
| pcap open | 10 ms timeout, immediate mode, nonblocking | An empty read returns instead of sitting for 10 ms |
| Membrane, measured | 2 MB in 0.42 s | 4.8 MiB/s. ~10 ms for 50 kB |
| Npcap, measured | 2 MB in 21.6 s | 0.09 MiB/s. ~0.5 s for 50 kB |
| UDP/RTP hole | Picture deadline is one frame, at least 2 ms | The late slice is dropped |
| AVB shape | 200 Mbit/s, 250 µs class B | 2 ms for 50 kB, 8 ms for 200 kB |
| ST 2110 pace | First row at the start of the frame, last row at the end | The whole frame interval |
| Opus | 480 samples at 48 kHz | 10 ms, beside the picture |
| ST 2110 audio | Level A PCM | 1 ms packets |

Wire time on a gigabit NIC is about 12 µs for a 1500-byte frame. It is not the term that decides 60 fps. The window, the syscall, the codec, and the two refreshes are.

## Links

| Rank | Path | Cost | Best when |
| --- | --- | --- | --- |
| 1 | Membrane NDIS, `\\.\MembraneNdis` | 4.8 MiB/s measured, later 6.3–7.7 MiB/s | Windows LAN, driver already open |
| 2 | Linux `poll` on libpcap | No 1 ms send wait | Linux raw socket |
| 3 | UDP | No raw NIC. Service port for QUIC, discovered RTP port for carriers 3–5 | Phone, or Ethernet will not open |
| 4 | macOS BPF + AF_NDRV | Host daemon opens the device | Mac on the LAN |
| 5 | Npcap, or libpcap where it is the fallback | 0.09 MiB/s measured on that Windows copy | Membrane is not open |

Opening Membrane does not start the driver. Win32 errors 2 and 3 mean the device is absent, and Npcap is the fallback. Linux libpcap with a blocking read was the multi-second stall; the poll loop is what replaced it. Do not read the Npcap row as a Linux number.

## Carriers

| Rank | Carrier | Cost | Best when |
| --- | --- | --- | --- |
| 1 | QUIC datagrams | 1 ms ACK, 256 KiB, drop a hole | LAN picture, both sides agreed |
| 2 | UDP/RTP | No Quinn. One-frame picture hole. `gcm` unless `plain` is set | LAN, no QUIC socket |
| 3 | QUIC streams | 25 ms ACK cap, 14,720-byte start, reliable | Long path, old peer |
| 4 | AVB, EtherType `0x22F0` | 200 / 2 / 1 Mbit/s, 250 µs class B, reservation or the session fails | Reserved LAN, picture inside 200 Mbit/s |
| 5 | ST 2110 | Paced across the frame. ~2.5 Gbit/s for uncompressed 1080p60 | 10 Gbit/s, JPEG XS or uncompressed |

Cursor samples on the datagram carrier travel with the picture records, not on the reliable picture stream. Pointer and pad on UDP/RTP are sent once. AVB input is the 1 Mbit/s class A stream only when that reservation succeeds.

## Codecs

| Rank | Codec | Catalog delay | Best when |
| --- | --- | --- | --- |
| 1 | PyroWave | ~1 frame | Both ends have GPU compute, the link can take the bits |
| 2 | MJPEG | ~1 frame | CPU encode that finishes inside the frame. Dirty tiles optional |
| 3 | JPEG XS | ~1 frame | ST 2110, or a studio link that is not carrying raw video |
| 4 | OpenAPV | ~1–2 frames | Intra when XS is not there |
| 5 | MJ2K | ~1–2 frames | Smaller than MJPEG, still intra |
| 6 | H.264 | ~2 frames | Hardware encode, link under a few tens of Mbit/s |
| 7 | H.265 | ~2–3 frames | Hardware encode, 30 or 60, not 144 |
| 8 | AV1 | ~3–4 frames | Smallest stream. Not 120 or 144 |

B-frames stay off. H.264 uses `zerolatency` / NVENC `ull` / AMF `ultralowlatency` / QSV lookahead 0. A long GOP is the dirty-rect path for H.264, H.265, and AV1, still with no B-frames. PyroWave still sends a full picture when something changed. Skip-unchanged applies to every codec.

## What to pick

| Goal | Link | Carrier | Codec | Rate |
| --- | --- | --- | --- | --- |
| 60 fps on a Windows LAN | Membrane | QUIC datagrams | GPU PyroWave, or hardware H.264 if the link is small | 60 or Match |
| 60 fps on a Linux LAN | poll L2 | QUIC datagrams | Same | 60 or Match |
| 120 or 144 | Membrane or Linux poll, display at that rate | QUIC datagrams or UDP/RTP | A ~1 frame codec that actually finishes inside a few milliseconds | Match |
| Phone, or no raw NIC | UDP | QUIC datagrams if the first RTT is under 2 ms, otherwise streams | Hardware H.264 or MJPEG | 60 if the RTT allows, else 30 |
| Reserved LAN, no encryption | Ethernet | AVB, after the reservation | A picture that fits in 200 Mbit/s | 60 |
| Studio | 10 Gbit/s | ST 2110 | Uncompressed, or JPEG XS | The link's frame rate |
| Long path | UDP | QUIC streams | H.264, H.265, or AV1, hardware | 30 or 60 |

120 and 144 are open in the offer, up to 240. Nothing in the short-path timer, the 1 ms ACK floor, or the datagram window forbids them. They lose when Auto is left on, the display is 60 Hz, Npcap is the open device, the carrier is reliable streams, or the codec's catalog delay is more than one frame.
