# etherquic-video

Copy a file over the etherquic L2 path. The bytes are the file itself, not LOC objects. `etherquic-video-send` writes them on one unidirectional QUIC stream. `etherquic-video-recv` writes that stream to disk.

The default service id is **4433**. The sender sets stream priority 0, below audio (10) and input (20).

From the `etherquic` directory. The binaries link Npcap the same way as the other L2 tools (`-L native=` to `wpcap.lib`).

```bash
cargo run --manifest-path video/Cargo.toml --bin etherquic-video-recv -- --nic IFACE --local-mac 02:00:00:00:00:01 --out received.mp4
cargo run --manifest-path video/Cargo.toml --bin etherquic-video-send -- --nic IFACE --local-mac 02:00:00:00:00:02 --peer-mac 02:00:00:00:00:01 --file video.mp4
```

The receiver writes `<out>.ready` before it accepts a connection and `<out>.done` with the byte count after the stream finishes. `ETHERQUIC_CERTS` selects the server certificate directory. The default is `etherquic/certs`.
