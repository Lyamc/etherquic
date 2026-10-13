# etherquic-input

Keyboard, mouse, and gamepad events over the etherquic L2 path. The recording format is EQIN v1, a self-delimiting little-endian stream on one unidirectional QUIC stream. This is the event layer of an RDP, VNC, or Remote Play style session. The crate records and carries events. It does not inject them into the OS.

Quinn sends a higher stream priority first. On a shared connection the order is input, then audio, then video:

| Stream | Priority |
|--------|----------|
| input events | 20 |
| audio | 10 |
| video | 0 |

`etherquic-input-send` calls `set_priority(20)` before it writes. Data already queued keeps the priority it was queued at, so the call has to come first. Video is 4433, audio is 4434, and input is **4435**. Those are separate processes today. The priorities are what a later shared connection uses to keep this order.

The QUIC stream is reliable. The lossy step is `coalesce`, and it runs only on events that have not been written yet. A later absolute pointer replaces the earlier one, pointer deltas saturate-add (`dx`, `dy`, and both wheels; buttons and time come from the later event), and a gamepad snapshot replaces the earlier snapshot of the same index. The surviving event stays at the later position, so a key that arrived between them is kept. Keys, lock-sync, and unknown kinds are never folded. An absolute pointer is not combined with a delta.

From the `etherquic` directory. The binaries link Npcap the same way as `etherquic-video-send` (`-L native=` to `wpcap.lib`).

```bash
cargo run --manifest-path input/Cargo.toml --bin etherquic-input-recv -- --nic IFACE --local-mac 02:00:00:00:00:01 --out received.eqin
cargo run --manifest-path input/Cargo.toml --bin etherquic-input-send -- --nic IFACE --local-mac 02:00:00:00:00:02 --peer-mac 02:00:00:00:00:01 --file events.txt
```

A file that starts with `EQIN` is binary. Anything else is a UTF-8 script, one event per line. `#` starts a comment. `t=MICROS` is optional and is not filled in by the sender. `--out received.txt` writes the script; any other extension writes EQIN. The receiver also prints each event. `.ready` and `.done` sidecars match the video and audio tools. `ETHERQUIC_CERTS` selects the server certificate directory. The default is `etherquic/certs`.

```text
t=1000 key down hid 0x00070004
key up set1 0x11D
key down keysym 0x61
pointer 32768 32768 left+right wheel=1 hwheel=-1
delta -10 4
pad 0 south+east 0 0 0 0 32767 0
sync caps num scroll kana
```

`hid` is a USB HID usage (high 16 bits page, low 16 bits usage; keyboard A is `0x00070004`). `set1` is an RDP scancode set 1 value with bit 8 (`0x100`) set for an extended key. `keysym` is an X11 keysym, the same space VNC uses.

`pointer` coordinates are `0..=65535` from the top left. Buttons follow the VNC mask: bit 0 left, bit 1 middle, bit 2 right, bit 3 x1, bit 4 x2. Names are `left`, `middle`, `right`, `x1`, `x2`, joined with `+`. `wheel` positive is away from the user. `hwheel` positive is to the right. `delta` uses the same buttons and wheels, with `dx` and `dy` as `i16`.

`pad` is one gamepad. Buttons follow the W3C and SDL standard order: South/A, East/B, West/X, North/Y, LB, RB, Back, Start, Guide, LStick, RStick, then d-pad up, down, left, right. The six integers after the buttons are `lx ly rx ry lt rt`. Sticks use the full `i16` range, positive X to the right and positive Y down. Triggers use `0..=32767` when pressed. `lx=0` style axes work too. `sync` carries the lock state: `caps`, `num`, `scroll`, `kana`.

An unknown kind is preserved. On the wire each event is `u16` payload length, then `u64` time in microseconds, a kind byte, and the body. The length counts the time, the kind, and the body.
