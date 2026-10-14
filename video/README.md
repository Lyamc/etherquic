# etherquic-video

Copy a file on one unidirectional QUIC stream. The bytes are the file, not LOC objects. Service **4433**. Priority 0, below audio (10) and input (20).

```bash
cargo run --manifest-path video/Cargo.toml --bin etherquic-video-recv -- --nic IFACE --local-mac 02:00:00:00:00:01 --out received.mp4
cargo run --manifest-path video/Cargo.toml --bin etherquic-video-send -- --nic IFACE --local-mac 02:00:00:00:00:02 --peer-mac 02:00:00:00:00:01 --file video.mp4
```

The receiver writes `<out>.ready` before accept and `<out>.done` with the byte count after the stream ends. `ETHERQUIC_CERTS` defaults to `etherquic/certs`. On Windows, `RUSTFLAGS` must point at the Npcap import library.
