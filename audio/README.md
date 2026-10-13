# etherquic-audio

Opus over the etherquic L2 path. Packets are LOC objects from [`etherquic-container`](../container/README.md) ([draft-ietf-moq-loc](https://github.com/moq-wg/loc) section 4.1).

One Ogg Opus packet is one object. `group_id` increments per packet, `object_id` is 0. The first object carries timescale `48000` and the `OpusHead` identification header. Every object carries a timestamp: the granule position of the packet's first sample, in samples at 48 kHz. The payload is the Opus packet itself (the TOC sequence), not an Ogg page.

The objects are concatenated on one unidirectional QUIC stream. The draft's "one MOQT group per QUIC stream" mapping needs a MOQT session; this crate does not have one, so the container's own length prefixes separate the objects.

From the `etherquic` directory. The binaries link Npcap the same way as `etherquic-video-send` (`-L native=` to `wpcap.lib`).

```bash
cargo run --manifest-path audio/Cargo.toml --bin etherquic-audio-recv -- --nic IFACE --local-mac 02:00:00:00:00:01 --out received.opus
cargo run --manifest-path audio/Cargo.toml --bin etherquic-audio-send -- --nic IFACE --local-mac 02:00:00:00:00:02 --peer-mac 02:00:00:00:00:01 --file clip.opus
```

The default service id is **4434** (video uses 4433). `--out received.loc` writes the LOC object stream instead of remuxing Ogg. `--format opus` or `--format loc` overrides the extension.

Input is an `.opus` / Ogg Opus file, or a LOC object stream. WAV and other codecs are not encoded here. Comment tags are not carried; the remuxed file has a short `etherquic-audio` vendor string. Ogg end-trim (a crop of the last page) is not a LOC property, so the remuxed file keeps the whole last packet.

Both directions are real binaries: `etherquic-audio-send` reads a source and writes one QUIC stream, `etherquic-audio-recv` reads that stream and writes a file. `--virtual` adds a userspace sound cable (`EQVS`) for machines with no audio device. The receiver plays its output file into the cable. The sender captures that cable instead of `--file`. The bytes are the Ogg Opus or LOC buffer, not PCM, so no system mixer and no extra codec are involved. `--virtual` alone uses `etherquic-audio.eqvs` in the current directory. The sender waits up to 60 seconds for the receiver to publish it.

```bash
cargo run --manifest-path audio/Cargo.toml --bin etherquic-audio-recv -- --nic IFACE --local-mac 02:00:00:00:00:01 --out received.opus --virtual
cargo run --manifest-path audio/Cargo.toml --bin etherquic-audio-send -- --nic IFACE --local-mac 02:00:00:00:00:02 --peer-mac 02:00:00:00:00:01 --virtual
```

`ETHERQUIC_CERTS` selects the server certificate directory. The default is `etherquic/certs`.
