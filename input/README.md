# etherquic-input

Keyboard, mouse, and gamepad events on one unidirectional QUIC stream. Format is EQIN v1, little-endian. The crate carries events. It does not inject them.

Service **4435**. Priority 20, ahead of audio (10) and video (0). `set_priority(20)` runs before the first write. Data already queued keeps the priority it was queued at.

`coalesce` runs only on events not yet written. A later absolute pointer replaces the earlier one. Pointer deltas saturate-add (`dx`, `dy`, both wheels; buttons and time come from the later event). A gamepad snapshot replaces the earlier snapshot of the same index. The survivor stays at the later position, so a key between them is kept. Keys, lock-sync, and unknown kinds are never folded. An absolute pointer is not combined with a delta.

```bash
cargo run --manifest-path input/Cargo.toml --bin etherquic-input-recv -- --nic IFACE --local-mac 02:00:00:00:00:01 --out received.eqin
cargo run --manifest-path input/Cargo.toml --bin etherquic-input-send -- --nic IFACE --local-mac 02:00:00:00:00:02 --peer-mac 02:00:00:00:00:01 --file events.txt
```

A file that starts with `EQIN` is binary. Anything else is a UTF-8 script, one event per line. `#` comments. `t=MICROS` is optional. `--out received.txt` writes the script. Any other extension writes EQIN. `.ready` and `.done` match the video tool. `ETHERQUIC_CERTS` defaults to `etherquic/certs`.

```text
t=1000 key down hid 0x00070004
key up set1 0x11D
key down keysym 0x61
pointer 32768 32768 left+right wheel=1 hwheel=-1
delta -10 4
pad 0 south+east 0 0 0 0 32767 0
sync caps num scroll kana
```

`hid` is a USB usage (page in the high 16 bits, usage in the low 16; keyboard A is `0x00070004`). `set1` is an RDP scancode set 1 value. Bit 8 (`0x100`) marks an extended key. `keysym` is an X11 keysym.

`pointer` is `0..=65535` from the top left. Buttons use the VNC mask: bit 0 left, 1 middle, 2 right, 3 x1, 4 x2, joined with `+`. `wheel` positive is away from the user. `hwheel` positive is to the right. `delta` uses the same buttons and wheels, with `dx` and `dy` as `i16`.

`pad` buttons follow the W3C / SDL order: South/A, East/B, West/X, North/Y, LB, RB, Back, Start, Guide, LStick, RStick, d-pad up, down, left, right. Then `lx ly rx ry lt rt`. Sticks are full `i16`, positive X right, positive Y down. Triggers are `0..=32767`. `lx=0` style axes work. `sync` is `caps`, `num`, `scroll`, `kana`.

On the wire: `u16` payload length, `u64` time in microseconds, a kind byte, then the body. The length counts the time, the kind, and the body. An unknown kind is kept.
