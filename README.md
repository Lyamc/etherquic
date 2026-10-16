# etherquic

QUIC over Ethernet. Quinn sees datagrams, an opaque peer, and a max payload. It does not see MAC addresses.

## Stack

| Piece | Where |
| --- | --- |
| QUIC | [Lyamc/quinn](https://github.com/Lyamc/quinn) submodule, `rustls-pure`, pin `a201190` |
| TLS | [Lyamc/rustls-rustcrypto](https://github.com/Lyamc/rustls-rustcrypto) submodule, pin `9cba6e9` (AES-128-GCM for QUIC) |
| PyroWave on Direct3D 11 | [Lyamc/pyrowave-dx11](https://github.com/Lyamc/pyrowave-dx11) submodule `vendor/pyrowave-dx11`, tag `v0.1.0`. Windows loads `pyrowave_d3d11.dll` when Vulkan is missing or only a CPU device answered |
| L2, first choice | NDIS filter `membrane_ndis`, device `\\.\MembraneNdis`. [Install](docs/windows-membrane-ndis.md) |
| L2, fallback | libpcap / Npcap (`src/pcap_link.rs`) when that device is not open |
| Tests | in-memory `Fabric` |

EtherType `0x88B6`. A frame carries destination service, source service, and payload length, so Ethernet padding does not swallow a short QUIC ACK. Rust 1.85. The crypto path has no `ring`, `aws-lc`, or `cc`. This crate is `#![forbid(unsafe_code)]`.

## Clone

The QUIC, TLS, and Direct3D 11 PyroWave trees are submodules. Relative URLs (`../quinn.git`, `../rustls-rustcrypto.git`, `../pyrowave-dx11.git`) follow the clone's protocol. Initialize them before `nix-build`.

```bash
git clone --recurse-submodules git@github.com:Lyamc/etherquic.git
git submodule update --init
```

## Send a file

```bash
cargo build --release --manifest-path video/Cargo.toml
sudo setcap cap_net_raw,cap_net_admin=ep ./video/target/release/etherquic-video-send
./video/target/release/etherquic-video-send \
  --nic IFACE --local-mac SENDER_MAC --peer-mac PEER_MAC --file video.mp4
```

On Windows the receiver tries NDIS, then Npcap:

```text
etherquic-video-recv --local-mac PEER_MAC --out received.mp4
```

Linux send needs `CAP_NET_RAW` and `CAP_NET_ADMIN`. On Windows, point `RUSTFLAGS` at the Npcap import library:

```powershell
$env:RUSTFLAGS = '-L native=PATH\TO\npcap\lib'
```

## NDIS and Npcap

Same copy, 1 Gbps link, 5–16 ms ping. NDIS is `membrane_ndis`. Npcap is what runs after that driver is stopped.

| Workload | NDIS | Npcap |
| --- | ---: | ---: |
| 115-byte note, median of 5, after connect | 0.05 s | 0.09 s |
| 2 MB, median of 3 | 0.42 s (4.8 MiB/s) | 21.6 s (0.09 MiB/s) |
| 18 MB video | 2.46 s (7.0 MiB/s) | still running at 180 s |

For the 2 MB file the receiver used about 0.016 s of CPU on NDIS and about 0.36 s on Npcap. A 1 ms idle wait later moved NDIS to 6.3–7.7 MiB/s (2 MB) and 9.2 MiB/s (video). One system call per frame is the gap. Round-trip time caps how full the QUIC window gets.

## Commands

```bash
cargo test
cargo run --bin etherquic-demo
cargo run --manifest-path ui/Cargo.toml --bin etherquic
cargo run --manifest-path play/Cargo.toml --bin etherquic-play -- host --nic IFACE
cargo run --manifest-path play/Cargo.toml --bin etherquic-play -- connect --nic IFACE --peer-mac 02:00:00:00:00:01
```

`etherquic` is the window: `host` and `connect`. The `ui` feature is the GPUI front end. Host and Connect run the live desktop from `etherquic-play`: the picture, the audio, and keyboard, mouse, and gamepad.

## Capture

Preferences has Capture. It writes `ether.pcap` and `app.pcap` under `$XDG_STATE_HOME/etherquic/capture/`, or `%LOCALAPPDATA%\etherquic\capture\` on Windows. `ETHERQUIC_CAPTURE=DIR` starts that for the window and for `etherquic-play`. Decode both with `wireshark/etherquic.lua`. Capture is off until it is turned on, and the switch is not saved.

## Crates

| Crate | What it does |
| --- | --- |
| [`ui`](ui/README.md) | Window. Taskbar button only while hosting. |
| [`play`](play/README.md) | Desktop, audio, and input on one connection. |
| [`input`](input/README.md) | Keyboard, mouse, gamepad. Service 4435, priority 20. |
| [`audio`](audio/README.md) | Opus as LOC objects. Service 4434, priority 10. |
| [`video`](video/README.md) | One file, one stream. Service 4433, priority 0. |
| [`container`](container/README.md) | LOC objects. |

On one connection the higher priority is sent first: input, then audio, then video.

Why Quinn can stay unaware of Ethernet: [plan.md](plan.md).
