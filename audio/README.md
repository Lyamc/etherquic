# etherquic-audio

Opus as LOC objects from [`etherquic-container`](../container/README.md). One Ogg Opus packet is one object: `group_id` increments, `object_id` is 0. The first object carries timescale `48000` and the `OpusHead` header. Every object carries a timestamp, the granule of the packet's first sample at 48 kHz. The payload is the Opus packet, not an Ogg page. Objects are concatenated on one QUIC stream. Service **4434**.

```bash
cargo run --manifest-path audio/Cargo.toml --bin etherquic-audio-recv -- --nic IFACE --local-mac 02:00:00:00:00:01 --out received.opus
cargo run --manifest-path audio/Cargo.toml --bin etherquic-audio-send -- --nic IFACE --local-mac 02:00:00:00:00:02 --peer-mac 02:00:00:00:00:01 --file clip.opus
```

`--out received.loc` writes the object stream. `--format opus` or `--format loc` overrides the extension. Input is `.opus` / Ogg Opus or a LOC stream. WAV is not encoded here. Comment tags are dropped. The remuxed file uses vendor `etherquic-audio` and keeps the whole last packet (Ogg end-trim is not a LOC property).

`--virtual` uses a userspace cable (`EQVS`) when the machine has no audio device. The bytes are still the Ogg or LOC buffer. Alone, the cable file is `etherquic-audio.eqvs` in the current directory. The sender waits up to 60 seconds for the receiver to publish it.

`ETHERQUIC_CERTS` defaults to `etherquic/certs`. On Windows, `RUSTFLAGS` must point at the Npcap import library.
