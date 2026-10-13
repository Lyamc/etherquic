# etherquic

Experimental **QUIC over Ethernet**, with Quinn kept unaware of L2.

## Stack

| Piece | Source |
|-------|--------|
| QUIC | [Lyamc/quinn](https://github.com/Lyamc/quinn) (`rustls-pure`) |
| TLS crypto | vendored `rustls-rustcrypto` (same provider as Lyamc/moq-rs) |
| L2 I/O, primary | Rust NDIS filter `membrane_ndis` on Windows (`src/ndis_link.rs`, device `\\.\MembraneNdis`) |
| L2 I/O, backup | **libpcap / Npcap** (`src/pcap_link.rs`) when that device is not open |
| In-memory fabric | `Fabric` for same-process tests |

EtherType **`0x88B6`** (borderless/membrane stay on `0x88B5` / `0x8888`). Frames carry
`dst_service`, `src_service`, and **`payload_len`** so Ethernet minimum-size padding
does not corrupt short QUIC ACKs.

Datagram bodies are copied with `copy_from_slice` / `to_vec`. The crate builds on
stable Rust (`rust-version` 1.85). No `ring`, no `aws-lc`, and no `cc` on the crypto
path. This crate itself is `#![forbid(unsafe_code)]`.

## Video over L2

```bash
# Linux (needs CAP_NET_RAW): setcap then send
sudo setcap cap_net_raw,cap_net_admin=ep ./target/release/etherquic-video-send
./target/release/etherquic-video-send \
  --nic enp4s0 --local-mac d8:5e:d3:88:5e:0e \
  --peer-mac a8:a1:59:e3:e2:69 --file video.mp4

# Windows: receive. NDIS (\.\MembraneNdis) is tried first; Npcap is the fallback.
etherquic-video-recv --local-mac a8:a1:59:e3:e2:69 --out received.mp4
mpv received.mp4
```

## NDIS vs Npcap

Same transfer, Linux sender on `enp4s0`, HYTE receiver. NDIS is `membrane_ndis`.
Npcap is the fallback after that driver is stopped, on the Killer adapter.
Ping between the hosts was 5–16 ms and this machine's Ethernet link was 1 Gbps,
so the frames crossed the mesh between the Ethernet ports rather than a direct
2.5-gigabit switch.

| Workload | NDIS | Npcap |
| --- | ---: | ---: |
| 115-byte note, median of 5, after the QUIC connection was up | 0.05 s | 0.09 s |
| 2 MB file, median of 3 | 0.42 s (4.8 MiB/s) | 21.6 s (0.09 MiB/s) |
| 18 MB video | 2.46 s (7.0 MiB/s) | still running at the 180 s limit |

The Windows receiver used about 0.016 s of CPU and 11–12 MB for the 2 MB file on
NDIS, versus about 0.36 s of CPU and 13 MB on Npcap. Shortening the I/O thread's
idle wait from 5 ms to 1 ms later raised the NDIS rates to 6.3–7.7 MiB/s (2 MB)
and 9.2 MiB/s (video). The byte copy is not what separates the two backends. One
system call per frame still dominates, and the mesh RTT caps how full the QUIC
window can get.

## Quick start

```bash
cargo test
cargo run --bin etherquic-demo
cargo run --manifest-path audio/Cargo.toml --bin etherquic-audio-send -- --file clip.opus ...
cargo run --manifest-path audio/Cargo.toml --bin etherquic-audio-recv -- --out received.opus
cargo run --manifest-path input/Cargo.toml --bin etherquic-input-send -- --file events.txt ...
cargo run --manifest-path input/Cargo.toml --bin etherquic-input-recv -- --out received.eqin
cargo run --manifest-path ui/Cargo.toml --bin etherquic-ui
```

`etherquic-ui` is the host / player console. Linking any L2 binary needs Npcap on the library path (`$env:RUSTFLAGS = '-L native=C:\Build\networking\borderless\third_party\npcap\lib'`). See [`ui/README.md`](ui/README.md).

## Media

- [`ui`](ui/README.md) — host / player console (`etherquic-ui`). The taskbar icon is present only while this PC is hosting.
- [`input`](input/README.md) — keyboard, mouse, and gamepad events (`etherquic-input-send` / `etherquic-input-recv`)
- [`container`](container/README.md) — LOC objects ([draft-ietf-moq-loc](https://github.com/moq-wg/loc))
- [`audio`](audio/README.md) — Opus packets wrapped in those objects (`etherquic-audio-send` / `etherquic-audio-recv`)
- `etherquic-video-send` / `etherquic-video-recv` — copy a video file on one QUIC stream

On a shared connection Quinn sends a higher stream priority first: input (20), then audio (10), then video (0).

## Layout

- `src/frame.rs` — EtherType + dst/src service ids + opaque QUIC payload
- `src/fabric.rs` — in-memory L2 segment
- `src/socket.rs` — Quinn `AsyncUdpSocket` / `UdpSender`
- `src/addr.rs` — MAC ↔ IPv6 link-local `SocketAddr`
- `src/tls.rs` / `src/endpoint.rs` — pure TLS + endpoint helpers
- `certs/` — demo server certificate (skip-verify on the client)
- `vendor/quinn` — Lyamc/quinn with `rustls-pure`
- `vendor/rustls-rustcrypto` — pure CryptoProvider (+ QUIC AES-128-GCM)

## Will it work if QUIC is unaware it sits on Ethernet?

Yes: Quinn only sees datagrams, opaque peers, and a clamped max payload. See `plan.md`.
